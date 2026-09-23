# lane-av1-loss64 — lossless sb128 full-frame exact, including skipped sub-8x8 intrabc chroma

Base: `main` **ed12fe52** (rect14 merged). Worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-av1l64`, branch
`lane-av1-loss64`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1loss64`,
`TMPDIR=$HOME/.cache/tmp-av1loss64`. Nothing pushed.

## 1. Headline

The lane closes the lossless `sb-size=128` 320x240 acceptance arm at
full-frame sample exactness. It also fixes the remaining 4:8/8:4 sub-8x8
chroma prediction defect in a skipped intrabc leaf. The first source fix is
committed as `0226fc11` (`ec-av1: clip lossless chroma TUs and reset skip bands`);
the skipped-intrabc chroma fix and its gate are the follow-up commit on this
branch.

Verified gates:

```
a_lossless_sb128_square_frame_clips_overhanging_chroma_tus
  2 arms full-frame exact, 192 chroma units clipped

a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands
  full-frame exact, 1 band resets, 3 skipped intrabc chroma predictions
```

The first gate is the required 320x240 lossless sb128 full-frame witness. The
second is a 320x242 sibling generated with `--min-partition-size=4`; it reaches
the sub-8x8 VERT -> 4x8 skipped-intrabc leaf and is also full-frame exact.

## 2. Root causes

Two independent defects were present in the cancelled WIP:

* A lossless chroma transform unit at a frame edge was read with the nominal
  geometry instead of libaom's clipped per-plane geometry. The decoder could
  read or reconstruct outside the valid chroma plane footprint.
* In `decode_leaf_rect8`, a 4:8/8:4 chroma-reference leaf with `skip_txfm=1`
  and `use_intrabc=1` was sent through the ordinary skipped-intra branch.
  That branch predicts chroma with DC/CfL, but libaom still reconstructs a
  skipped intrabc block from its motion-compensated copy; skip only suppresses
  residual coefficient reads. The entropy ladder remained synchronized while
  the first wrong sample was U(104,92) at luma mi(46,52).

For 4:2:0, libaom clamps a 4x8/8x4 leaf's chroma plane block to `BLOCK_4X4`:
`set_plane_n4` in `av1_common_int.h:1344` and `get_plane_block_size` in
`blockd.h:1184`. The fix therefore predicts a 4x4 U and V block at the group's
chroma origin `(cpx, cpy)`, using the intrabc DV, and writes no chroma
coefficients on the skipped route. An unskipped intrabc leaf keeps the ordinary
`Chroma4` coefficient walk over the same 4x4 prediction. Both routes disarm
`intrabc_chroma_tx`.

The lossless skip-band fix resets the complete luma entropy-band footprint
before the next block. Its measured sibling previously left a stale band and
read the next `txb_skip_ctx` differently from libaom.

## 3. Repro and exactness

Oracle: `~/.cache/aom-oracle/build/aomenc`; pixel reference: ffmpeg.

The sub-8x8 witness was generated from `testsrc2=s=320x242:r=25`, one frame,
with `--lossless=1 --sb-size=128 --enable-1to4-partitions=1
--min-partition-size=4 --obu`. The committed-lane release probe and ffmpeg
reference have identical decoded bytes (`diff 0`). Artifact hashes:

```
obu:  1fe5edc300a378c486abe385dad8251f3f21452f04e5a6f60f69feb48e58c8cb
ref8: 421ca38c91a133e78b84a9a3c9f79defbddbc7163a0b3088a176f4cd50179728
ours: 421ca38c91a133e78b84a9a3c9f79defbddbc7163a0b3088a176f4cd50179728
```

Before the chroma-route fix, Y was exact but U/V differed at 2,488 samples;
the first difference was U(104,92). After the fix, all Y/U/V samples are exact.
The gate reports three reached skipped-intrabc chroma predictions and one
lossless band reset, so neither fix is gate-blind.

## 4. Changed paths

* `crates/ec-av1/src/decode.rs`: clip lossless chroma TU geometry; clear the
  skipped lossless entropy bands; route skipped sub-8x8 intrabc chroma through
  clamped 4x4 motion-compensated prediction; add the engagement hit used by the
  gate.
* `crates/ec-av1/src/stream.rs`: change the lossless skip-band gate to the
  min-partition-size=4 witness and assert both the band-reset and skipped
  intrabc-chroma counters.

No refusal string, encoder path, or unrelated decode seat was changed.

## 5. Verification run

Targeted commands, all with private lane target and temp directories:

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
  a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands -- --nocapture
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
  a_lossless_sb128_square_frame_clips_overhanging_chroma_tus -- --nocapture
```

Both passed. The release `decode_probe` was rebuilt and independently compared
to ffmpeg for the 320x242 min4 witness; all planes were byte exact.

The project-wide suite, 0-warning parity sweep, and the cross-lane acceptance
arms (mono, 10-bit `hg_*`, CDF-disabled, 4:4:4 refusal, qmatrix refusal) are
owned by the main agent and were not run in this lane.

## Suite (orchestrator-run, committed tree 5c48d7b0)

`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1l64 TMPDIR=$HOME/tmp-av1l64 cargo test -p ec-av1 --release --lib -- --test-threads=1`:

```
test result: ok. 605 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 1371.22s
```

605 = 603 (main after rect14) + this lane's 2 new gate tests. First attempt
failed 6 tests on a missing TMPDIR directory (`writing the probe stream: No
such file or directory`) — environmental, green on re-run with the dir
created.

## Merge-close (main, merge commit 0b12fa9f)

Merged as **0b12fa9f** (`Merge lane-av1-loss64: lossless sb128 full-frame
exact + skipped sub8 intrabc chroma`), parents 5960a805 (main with the
intrabc merge) + 0d0ad1c2 (lane head). Git auto-merged all four touched paths
(ort, zero textual conflicts); one SEMANTIC conflict was resolved inside the
merge commit:

* The intrabc merge rewired `decode_block_rect4`'s 1:4 intrabc strips from
  `decode_intrabc_owned_rect` (this lane's base) to `decode_intrabc_rect`,
  whose skip arm lacked the band reset this lane added only to
  `decode_intrabc_owned_rect`'s. Consequence, traced with a strip-entry
  eprintln on both trees: the lane tree visits exactly one rect intrabc
  strip (`mi=(48,66) 8x16 skip=true`); the merged tree, running with stale
  entropy bands, mis-parsed the NEXT partition as a phantom unskipped 8x32
  intrabc strip at `mi=(56,66)` and died on its chroma — a lossless (4,16)
  unit hitting `TxParams::run`'s 4x4 WHT assert (`left: (4, 16)`).
* Resolution (inside the merge, per the principle that the fix lives in
  whichever function serves the call sites): `decode_intrabc_rect`'s skip
  arm gains the same lossless-scoped `record_mi_luma_rect` walk (the lossless
  arm of `read_block_tx_size_rect` resolves leaves before the skip check, so
  the walk only ever fires for lossless strips). `decode_intrabc_owned_rect`
  at `decode_block_rect64` keeps its copy. Both loss64 gates then pass on
  the merged tree with byte-identical summaries.
* The gate binary was rebuilt and `cargo check -p ec-av1 --all-targets`
  re-run after the fix: 0 warnings.

### Merged-tree gates (private `CARGO_TARGET_DIR`/`TMPDIR` under `$HOME`)

```
a_lossless_sb128_square_frame_clips_overhanging_chroma_tus
  2 arms full-frame exact, 192 chroma units clipped
  test result: ok. 1 passed; 0 failed; 665 filtered out

a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands
  full-frame exact, 1 band resets, 3 skipped intrabc chroma predictions
  test result: ok. 1 passed; 0 failed; 665 filtered out

a_coded_rect_intrabc_block_reconstructs_in_both_orientations
  (EC_AV1_RECON_THREADS=1 AND =4, all four vert-skip arm lines:
   pal0 horz=11 vert=2, pal1 horz=1 vert=2 at both thread counts)
  test result: ok. 1 passed; 0 failed; 665 filtered out  (both runs)

rect14's five gates (a_16x4_intrabc_pair_strip_decodes_pixel_exact,
  a_lossless_16x4_chroma_pair_repairs_the_measured_site,
  a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips,
  every_proven_refusal_names_a_test_that_exists,
  the_decode_path_refuses_exactly_the_listed_cases)
  test result: ok. 5 passed; 0 failed; 661 filtered out
  in-gate totals: 187 rect-strip use_intrabc reads, 5 intrabc blocks over
  5 arms, 0 refused, 8 frames compared, 3 out of scope (0 mismatched)

cargo test -p ec-av1 --lib -- --test-threads=1 refusal
  test result: ok. 21 passed; 0 failed; 645 filtered out; finished in 110.59s
```

### Merged-tree probes (release `decode_probe`, all `cmp` vs fresh ffmpeg oracles)

| probe | verdict |
|---|---|
| loss320 (320x240 lossless sb128 1to4 min4) — the headline | **FULL-FRAME EXACT 115200 B** (`rect4_16_pair: lossless_chroma=6`); pre-lane this stopped at luma row 128, first diff byte 41121 |
| sub8 (320x242 lossless sb128 min4) | **EXACT 116160 B** (`leaf8_intrabc_hits=2`, `intrabc_rect=1`) |
| A 256x192 cq45 pal0 txs1 (var-tx arm) | EXACT 73728 B (`intrabc_rect=1 var-tx=1`) |
| B 512x384 cq45 pal1 txs0 (coded 8x16) | EXACT 294912 B (`intrabc_rect=2`) |
| ibc640 (rect14 §2 recipe) | EXACT 460800 B (`rect4_16_pair: intrabc=1`) |
| cq50 arm (256x192 cq50 pal0 txs0) | EXACT 73728 B |
| mono 60f (`av1-monochrome.ivf` remux, OUT16 low byte) | EXACT 4608000 B vs ffmpeg gray |
| hg_* 8 committed 10-bit fixtures (`EC_PROBE_OUT16`) | 8/8 EXACT vs yuv420p10le |
| av1-profile1-444 | REFUSED by name (`a chroma format other than 4:2:0`), 0 bytes |
| aomenc `--enable-qm=1` / `=0` | qm=1 REFUSED by name (`a frame using quantisation matrices (using_qmatrix=1)`), 0 bytes; qm=0 control EXACT 73728 B |
| `--cdf-update-mode=0` 3-keyframe stream | EXACT 221184 B |

### Full suite on the merged tree

Hub-supervised (`suite-av1l64m`, `cargo` with `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1l64m TMPDIR=$HOME/.cache/tmp-av1l64m EC_AV1_REQUIRE_AOMENC=1 RUST_TEST_THREADS=1`), `cargo test -p ec-av1 --lib -- --test-threads=1` on the committed amended merge commit **0b12fa9f**:

```
running 666 tests
test result: ok. 606 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 9641.30s
```

Exit 0, no kills, no flake: 606 = 604 (main after the intrabc merge, its
603+1 evidence recorded in lanes/av1intrabc.report.md §6) + this lane's 2 new
gate tests. A first run over the pre-fix merge was killed after the sub8 gate
went red (the semantic conflict above); it was superseded by this run and its
result discarded.

## Follow-ups

* `accepted` — the mirrored sub-8x8 8x4 (HORZ) skipped-intrabc leaf route is
  fixed by the same code path but is not named by any committed witness: the
  gate's stream reaches the VERT -> 4x8 leaf (3 skipped chroma predictions).
  The lane report documents no open 4x8-leaf chroma corner beyond this; the
  axes-swapped witness would need an encoder recipe that codes a skipped
  HORZ 4:8 leaf (none in the corpus does).
* `accepted` — pre-existing lossy 128-axis luma class, first difference at
  byte 60369 on the reviewer's stream: the reviewer verified it is
  identical-on-base (present on 5960a805 before this merge), so it is not a
  merge regression. Unchanged by this lane.
* `deferred(unblock: a lossless stream that codes an unskipped 8x32-shaped
  1:4 intrabc strip)` — a latent lossless whole-unit
  chroma read shared by BOTH `decode_intrabc_rect` and
  `decode_intrabc_owned_rect`: an unskipped 8x32-shaped 1:4 strip in a
  lossless frame would read chroma as one (4,16) unit through the WHT
  assert. Unreachable in the current corpus (the correctly parsed lossless
  streams contain no unskipped 8x32 intrabc strip — the phantom one that
  reached it was itself the stale-band defect's product), and identical on
  both parents. Unblock: a lossless stream that codes one.
