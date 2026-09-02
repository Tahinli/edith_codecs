# lane-intra64split r4 — HANDOFF

## Tree
Branch `lane-intra64split`. Merged this round, in this order, each as its own
merge commit (`513d20b`..`0fea9e3`), all compiling (`cargo check -p ec-av1
--all-targets` clean):

* `main` 1176a16
* `a5b9770` lane-intersub8
* `7f372b9` lane-uv8
* `10b801a` lane-interp3
* `2e711e1` lane-inter16ab
* `48216c2` lane-rectchroma2
* `fedb7fe` lane-sb128c
* `e0c8bef` lane-intra14
* `a37a4bc` lane-intrasplit

Then `aebf4f8` (decode_probe prints the witness counter) and `c20a258`
(merge fallout: one OBMC refusal string, one retired palette refusal).

Resolution rules that a re-merge must repeat (they are also in the ledger):
* `TxbSet::LumaRect8x4Inter` is defined by BOTH intersub8 and inter16ab with
  identical semantics — keep ONE; keep inter16ab's `LumaRect8x4Inter1` next to
  intersub8's `LumaRect8x4InterSet1`.
* Wedge row selection: take inter16ab's `bsize_all_index` /
  `wedge_used_bsize` / `is_any_masked_compound_used_here` consistently; drop
  intersub8's `bsize_index` / `wedge_used_wh` twin of the same fix.
* OBMC sentinel refusal: the surviving decode test asserts "an OBMC neighbour
  whose interp filter was never recorded (no switchable symbol for that block)"
  — message, inventory entry and test must come from one branch.
* The refusal "a 1:4 rect strip that actually uses a palette" is NO LONGER
  emitted after the merge (palette on rect strips landed); it must be deleted
  from `refusal_inventory.rs` or the exact-set test fails.
* intra14 wraps the rect-strip intra reader's key-frame prologue in an `else`
  arm; main's `use_intrabc` read belongs INSIDE that arm.

## The refusal and what the decoder does behind it
The 64x32 / 32x64 depth-0 intra strip is NOT refused: the decoder decodes it
and counts it in `RECT64_CORNER_TU_HITS` (`crates/ec-av1/src/decode.rs:2182`,
incremented at `decode.rs:6652`, exposed as
`stream::rect64_corner_tu_hits(orient)` — 0 = 64x32, 1 = 32x64). What is still
missing is a WITNESS: no stream yet decodes to completion while that counter is
non-zero, so the two gate arms in `stream.rs`'s `intra_rect64_in_inter_gate`
stay `#[ignore]`d with r3's dated reason, and r3's tripwire
(`assert!(hits == 0)` on the pixel-exact synthetic recipe) is still armed. The
strips ARE still hit inside frames that refuse, on other lanes' refusals — see
the frontier table in `lanes/intra64split-r4.report.md`.

Coordinator's note (main 61aa768, 36-offset census) says the TU 64x32/32x64
refusal IS the first stop at 11 of 36 offsets. That is a DIFFERENT tree from
this branch: on this merged branch the `-ss 0` cut stops instead at "an inter
SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B ...)" after 33 dumped
frames — the 8 merged lanes moved the stop past the TU refusal. A successor
must re-measure the stop string on THIS branch before acting on the census.

## Film cuts already prepared (do not re-extract)
Scratchpad dir
`/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/i64r4/`
(tmpfs — re-extract with `ffmpeg -ss S -t 2 -c:v copy -an -f obu` if reaped):

* `s0.obu` 44718 B, `s300.obu`, `s1200.obu`, `s1800.obu`, `s3000.obu`,
  `s3600.obu` — the 2 s 2160p 10-bit cuts at those offsets.
* `t<N>.obu` — `s0.obu` truncated to the first N frame-carrying OBUs with
  `census4/trunc.py` (N = 1..73 sampled at 21 points). `t49.obu` (4161 B) is the
  LONGEST clean prefix: decodes with no refusal, `rect64_corner_tu = 0 / 0`.
  `t50.obu` (23472 B) is the first that refuses, and it is where all 15 hits
  (14 / 1) appear.
* `k<S>.obu` — key-frame-only prefix of each cut; all five decode clean with
  `rect64_corner_tu = 0 / 0`.
* `p<S>.log` — probe logs; `d<S>/f*` — `EC_AV1_FINAL_DUMP` frames.

## Exact probe command for the successor
```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intra64split EC_NOMEMGUARD=1 \
  nice -n 10 cargo build -p ec-av1 --example decode_probe -j3

EC_AV1_FINAL_DUMP=<dir>/f systemd-run --user --scope -q -p MemoryMax=6G \
  $HOME/.cache/cargo-target-intra64split/debug/examples/decode_probe <cut>.obu
```
`EC_AV1_FINAL_DUMP` is a PREFIX, not a directory: pass `<dir>/f`, else zero
frames are written and it looks like a total refusal. Read the witness off the
probe's own `rect64_corner_tu: 64x32=.. 32x64=..` line. Truncate with
`python3 <scratchpad>/census4/trunc.py <in>.obu <N> <out>.obu`.

## Suite
COMPLETE, RED. `intra64split-suite-r4b-1788350949`,
`$HOME/.cache/intra64split-suite-r4.log`:
`test result: FAILED. 405 passed; 3 failed; 39 ignored; 0 measured` in 798s.
The two failures the FIRST (pre-fix) run exposed are fixed and green.
The three remaining are merge cross-product reds owned by the merged lanes, not
by this one -- full attribution in `lanes/intra64split-r4.report.md`:

1. `a_frame_edge_straddling_band_decodes_pixel_exact` -- 192x68 cq61 8-bit,
   frame 1 Y 6859 px, first (row 0, col 64), ours 56 vs ffmpeg 178.
2. `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
   -- firing assert, split-tx-8x4 arm 0 of `[1,1,2,2,0]`, no attempt refuses on
   rectangular residual coding any more.
3. `a_real_aomenc_stream_with_a_rectangular_compound_wedge_decodes_pixel_exact`
   -- all 8-bit cq50 attempts still refuse "a COMPOUND_WEDGE mask on a
   non-square inter block".

A successor merging this branch anywhere must fix or explicitly own these three
first; they are the price of the eight-lane cross-product, and each is a gate
that is GREEN on its own lane tip.
