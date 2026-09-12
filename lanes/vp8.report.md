# lane-vp8 — native VP8 decoder (ec-vp8), RFC 6386

Base: `a8f2186a` (main), worktree `edith_codecs-vp8`, branch `lane-vp8`.
Never merged, never pushed. Pins untouched (no ec-av1 changes at all).

## Milestone table

| Milestone | State | Evidence |
|---|---|---|
| M1 bool decoder + frame tag + full first-partition header | **done/verified** | commit `57b5a53a`; 12 unit tests green; real vpxenc/ffmpeg streams (6 sizes/q, 8-token-partition, WebP still) parse with exact partition tiling, correct dims (incl. 76x52 non-MB-aligned), 0 desyncs |
| M2 key-frame-only decode, sample-exact vs ffmpeg | **root cause FIXED, verified at frame level** — decision-exact vs the reference model for the whole frame (19131 reads, 0 diffs) and frame-0 byte-exact vs ffmpeg on ALL 8 kf/mparts fixtures; the `keyframe_exact` suite goes 3/3 when inter frames land (each fixture carries 1 KF + inter frames), see M3 | commits `20f7d742`, `072f842f`, M2-fix commit (this one) |
| M3 inter frames | **major fix landed — gop frames 0-3 byte-exact; walled at frame 4+** — THREE M3 bugs fixed this session: (1) census above-left third-candidate merge is ALIVE (decodemv.c:366), (2) model hasy2 clobber for SPLITMV, (3) **long MV components must read bits 9,8,7,6,5,4 (mvlong_width=10) — bit 9 was missing**, which corrupted every large MV. Inter decode now: gop 4/60 frames exact, clip-obs 4/169, mparts 3/30, altref 1/90 (frame 0s + following); gop frame 1 at 95.3% then diverges. Remaining: next parse/reconstruct divergence from frame 4 (altref hits a hard decode Err at frame 35 — trace state committed). All traces (B/G/CNT/SP/TB/TBE/IMB/STAGE) in place for the next iteration | commits `151ef8ff`, `e01bec40`, `6152922e`, `e1eeec8d`, `8e1a04a9` |
| M4 public API + docs | **not attempted** | `Decoder::decode(&[u8]) -> Result<Option<Picture>>` exists; ec-av1-style `decode_stream` not yet |

## What shipped (M1 `57b5a53a`, M2 WIP `20f7d742`)

- `bool.rs`: RFC §7.3 decoder verbatim (bool/literal/signed/maybe-signed/
  prob7/tree + EOB-skipping token form), zero-extend init for tiny
  partitions, overread counter as desync detector; test-only RFC encoder.
- `frame.rs`/`header.rs`: 3-byte LE tag, key-frame start code/dims,
  complete partition-1 header in dixie field order (colour space,
  segmentation w/ cross-frame state, LF type/level/sharpness/deltas,
  token partition size table, quant indices, reference refresh/copies/
  sign-bias, coef-prob updates, mb_no_skip_coeff, inter prob_intra/last/gf
  + ymode/uv/MV updates). `PersistedState` carries entropy/segmentation/
  LF deltas across frames, resets at key frames.
- `tables.rs`: GENERATED from the RFC text by `scripts/extract_vp8_tables.py`
  (element counts asserted; page-header stripping; Pcat sentinels kept):
  default+update coefficient probs, kf_bmode_prob, coeff_bands, dequant
  lookups, Pcat1-6, default+update MV probs.
- `tokens.rs`: §13 GetCoeffs port — bands incl. sentinel entry, 3-context
  model (above/left per-plane, Y2 last-MB-with-Y2 semantics), EOB skipped
  after zero, dequant-on-write (DC at first coded position), Y2-first
  block order, dixie skip-MB reset (Y2 slots survive iff no Y2 block).
- `modes.rs`: all trees/tables; NOTE SPLITMV partition leaves use the RFC
  enum values (top_bottom=0, left_right=1, quarters=2, MV_16=3).
- `transform.rs` (subagent): idct4x4/iwht4x4 + shortcuts + dequant;
  400,000-vector bit-exact fuzz vs verbatim libvpx idctllm.c; dequant
  differentially fuzzed vs libvpx C over 528 q x 15 deltas x 5 channels.
- `intra.rs` (subagent): all 16x16/8x8/4x4 predictors; 20,000-stream
  machine diff vs libvpx intrapred.c — 28.8M/28.8M samples equal; svg2p
  RFC typo handled (avg2p).
- `loopfilter.rs` (subagent): normal+simple kernels and frame driver in
  dixie order (V-MB, V-sub, H-MB, H-sub; level 0 skips; combined
  blim/mblim for simple), dixie-table-exact limits, frame-type-exact hev
  via `filter_frame_ex(..., is_keyframe)`; 10 tests incl. Python-oracle
  full-frame driver order.
- `decode.rs`: per-row pipeline (modes -> tokens -> reconstruct),
  bordered planes (127 top incl. corner / 129 left), above-right
  synthesis (shared extras for right-edge subblocks, replicate on
  rightmost MB, 127 on top row), B_PRED interleaved predict+residual
  (subblocks chain through reconstructed pixels), Y2 WHT DC override,
  per-MB LF level with dixie DOUBLE-clamp, LF post-pass (order-equivalent
  to dixie's row-delayed filtering for non-threaded decode; frame-level
  level==0 gate).
- Fixtures: `scripts/gen_vp8_fixtures.sh` + local libvpx build recipe
  (`~/.cache/vp8/libvpx-src`, gives vpxenc for --token-parts; ffmpeg 8's
  wrapper has no token-part option). CQ quirk: `-crf` inside the
  qmin/qmax band is mandatory.

## Witnesses

- M1: `cargo test -p ec-vp8 --test header_parse` — 4 tests on real
  libvpx/vpxenc bitstreams: dims, start code, partition tiling
  (`token_data_offset + 3*(n-1) + sum(sizes) == frame len`), 8
  partitions, inter header coexists with persisted entropy, VP8-in-WebP
  chunk parses. ALL PASS.
- M2 (this session): FULL-FRAME decision-exactness — `EC_VP8_TRACE=1`
  example `dbg_first` vs `scripts/vp8ref_model.py`: 19131 token
  decisions, identical `(n, band, ctx, prob, bit)` on every read, 0
  diffs (`kf-160x96-q20.ivf`). Pixel exactness: frame 0 of all 7 kf
  fixtures + mparts dumped via `examples/dump_frame` and `cmp`-ed
  against `ffmpeg -frames:v 1` — ALL EXACT. `cargo test -p ec-vp8`:
  unit (49) + header_parse (4) + webp witness green; the remaining 2
  `keyframe_exact` cases decode every frame of each fixture and go
  green with M3 inter frames.

## M2 blocker — RESOLVED

Root cause: `get_coeffs`' ONE-token path (`tokens.rs`). After the
`p[2]` decision selects value 1, the reference walk (libvpx
`detokenize.c` GetCoeffs, RFC §13.2) falls through to the SHARED
sign-read / coefficient-write / EOB-after-nonzero sequence — the ONE
and magnitude paths converge right after the tree decision. The Rust
port had the sign/write/EOB block nested inside the magnitude branch
only, so a ONE token skipped the sign read and the EOB check and
re-entered the walk at the ZERO node: same read count (positions
stayed in lockstep, hence "same (pos,bit_count), different table
entry"), different decoded stream from the first ONE token in a busy
block — invisible on the lossless single-MB black fixture (no ONE
tokens), fatal from ~28 reads into kf-160x96-q20's first block.

Two reference-model bugs also fixed while instrumenting
(`scripts/vp8ref_model.py`): per-decision `G n/band/ctx/prob/bit`
lines replacing the DEC counter, and token-partition bool decoders
are now PERSISTENT across MB rows that share a partition (was:
re-created per row, which double-counted the walk on single-partition
streams). Trace protocol: diff `grep '^G '` of both sides; the first
differing line names decision, node and whether prob or bit diverges.


## Repro

```
CARGO_TARGET_DIR=~/.cache/cargo-target-vp8 cargo test -p ec-vp8
scripts/gen_vp8_fixtures.sh          # fixtures/vp8/*.ivf (gitignored)
```
