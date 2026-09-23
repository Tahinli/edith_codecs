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
ours: f8fc6c3248e6d8782ebccd88d46f7254aa8915fd4deaf62b7b92953599709eda
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
