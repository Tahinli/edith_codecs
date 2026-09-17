# lane-av1-intrabc r1 — the fifth gate-integrity entry closes with an engagement counter, not a flag

Branch `lane-av1-intrabc`, base `f715d70c`. Closes the last clause of
`debt|gate-integrity|ec-av1: five aomenc coding tools …` (deleted from
`~/.omp/agent/debts/DEBTS-home-tahinli-Documents-Code-Rust-edith_codecs.md`).

## What the debt premise got wrong at HEAD

`--enable-intrabc` is NOT off in all real-aomenc gates at `f715d70c`. lane-kf900 r6/r7 and
lane-t900 r31-r34 landed between 2026-09-02 and HEAD:

- `stream::tests::a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` —
  `--enable-intrabc=1`, assert `rect_intrabc_reads > 0 && intrabc_hits > 0 && leaf8_intrabc_hits > 0`,
  one named refusal expected (rect-strip intrabc).
- `stream::tests::an_sb128_screen_stream_with_intrabc_decodes_pixel_exact`
- `stream::tests::an_intrabc_block_under_tx_mode_select_decodes_pixel_exact`
- `stream::tests::a_sub8_leaf_census_over_intrabc_screen_streams_measures_the_sub8_refusal`
  and `an_intrabc_vartx_census_measures_the_mixed_leaf_refusal` (censuses).

Re-run at HEAD (all pass, one command, `--nocapture`):

    cargo test -p ec-av1 --release --lib -- --nocapture --test-threads=1 intrabc
    -> 7 passed; 0 failed; 1 ignored. `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`:
       185 rect use_intrabc symbol(s) read, 5 intrabc block(s) decoded across 4 arm(s),
       1 refused by name, 7 frames compared, 3 out of scope (0 mismatched), 1 intrabc block at an 8x8 leaf.

## The real hole this round found and closed: the 10-bit entry rested on a SPELLING

`gate_coverage.rs`'s 10-bit `enable-intrabc` closure cited
`a_real_aomenc_rect_strip_palette_decodes_pixel_exact`'s `smptebars-screen-txs-cq40/cq55` arms, which
merely pass `--enable-intrabc=1`. Measured with a temporary per-arm delta print: those arms decode
**0** intrabc blocks at both depths (all 5 arms × 2 depths), i.e. the entry was retired by a flag on a
command line — the exact defect the guard exists for.

Fix: `screen_intrabc_stream_at_depth` (depth-aware twin of `screen_intrabc_stream_with`) +
`an_sb128_screen_stream_with_intrabc_decodes_pixel_exact` now runs its four sb128 screen recipes at
8 AND 10 bits and hard-asserts a decoded intrabc block per depth (`blocks_at_depth[0] > 0 &&
blocks_at_depth[1] > 0`), every frame pixel-compared against ffmpeg's own decode at that depth
(`ffmpeg_decode_sequence_10bit` at 10 bits). Observed:

    sb128 8-bit  txs=0/1 × cq=30/45: intrabc_frames=1, intrabc_blocks=4/4/5/4, exact
    sb128 10-bit txs=0/1 × cq=30/45: intrabc_frames=1, intrabc_blocks=4/4/6/5, exact

Engagement is encoder-side too: libaom resets `allow_intrabc` when no block selected intrabc
(encodeframe.c:2335-2337), so a frame header that keeps `allow_intrabc=1` proves the encoder picked
it; the gate asserts `intrabc_frames > 0` off the parsed header as well.

`gate_coverage.rs` comments updated: the 8-bit paragraph records the re-verified counts; the 10-bit
paragraph records that the palette-gate citation was a measured zero-engagement spelling and names
the new closing witness. Case closed in report.

`decode::intrabc_hits()` is now exposed as `ec_av1::stream::intrabc_hits()` and printed by
`examples/decode_probe.rs`, so the standalone pair below prints the counter.

## Standalone command pair (prints engagement + comparison)

Fixture (outside git, `~/.cache/intrabc-fixture/`):

    ffmpeg -v error -f lavfi -i 'smptebars=size=128x96:rate=25' -t 0.2 -vf tile=2x2 \
      -pix_fmt yuv420p10le -strict -1 -f yuv4mpegpipe - \
    | ~/.cache/aom-oracle/build/aomenc --codec=av1 --bit-depth=10 --input-bit-depth=10 --passes=1 \
      --end-usage=q --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1 --limit=1 --threads=1 \
      --tile-columns=0 --min-partition-size=8 --max-partition-size=32 --sb-size=64 \
      --tune-content=screen --enable-intrabc=1 --enable-palette=0 --cq-level=30 \
      --enable-tx-size-search=1 --enable-rect-partitions=0 --enable-1to4-partitions=0 --sb-size=128 \
      --obu -o sb128-10bit-cq30.obu -

Engagement + result:

    EC_PROBE_HDR=1 cargo run --release --example decode_probe -- sb128-10bit-cq30.obu
    -> HDR 0: type=Key … screen_tools=true intrabc=true … tx_mode=Select
       OK: 1 frames decoded, 256x192 ; intrabc_hits: 6
    cargo run --release --example dump_yuv -- sb128-10bit-cq30.obu ours
    ffmpeg -v error -i sb128-10bit-cq30.obu -pix_fmt yuv420p10le -f rawvideo ref.yuv
    cmp ours.f0.yuv ref.yuv   -> BYTE-EXACT-10BIT
       sha256 de0d83756d22e422b19413e9e1c20325b9bd68fe2f27aae872da2d4736f6a007 (both files)

The arbiter agrees, and the oracle's own decoder accepts the stream
(`aomdec --i420 --output-bit-depth=10 --md5`: md5 `f1b7222466d15d1dda3e25fdbd8577fe`).

## Refusals — genuine capability gaps, all pinned

- `intra block copy on a HORZ/VERT/1:4 rect intra strip (reconstruction is not ported at this shape)` —
  the reader consumes the symbol and refuses by name; gate asserts exactly one such refusal
  (`a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`, `refused == 1`).
- sub-8x8 leaf intrabc and intrabc var-tx mixed-leaf shapes have their own censuses that assert the
  premise is reached before reading anything into the refusal firing. Current run: var-tx census
  decoded 5 intrabc blocks through the var-tx tree, 0 mixed-leaf resolutions, 4 frames exact.
  No bug localized; no arm with a decoded intrabc block mismatched ffmpeg (0 mismatches).

## Gates run

    EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib -- --nocapture --test-threads=1 intrabc
      -> 7 passed / 0 failed / 1 ignored
    cargo test -p ec-av1 --release --lib -- --exact gate_coverage::tests::never_exercised_8bit_matches_the_gate_recipes gate_coverage::tests::never_exercised_10bit_matches_the_gate_recipes
      -> ok (derived hole set unchanged: 8-bit 5 holes, 10-bit 2; `enable-intrabc` in neither)
    cargo check -p ec-av1 --all-targets -> 0 warnings
