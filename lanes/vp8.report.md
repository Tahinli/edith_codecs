# lane-vp8 — native VP8 decoder (ec-vp8), RFC 6386

Base: `a8f2186a` (main), worktree `edith_codecs-vp8`, branch `lane-vp8`.
Never merged, never pushed. Pins untouched (no ec-av1 changes at all).

## Milestone table

| Milestone | State | Evidence |
|---|---|---|
| M1 bool decoder + frame tag + full first-partition header | **done/verified** | commit `57b5a53a`; 12 unit tests green; real vpxenc/ffmpeg streams (6 sizes/q, 8-token-partition, WebP still) parse with exact partition tiling, correct dims (incl. 76x52 non-MB-aligned), 0 desyncs |
| M2 key-frame-only decode, sample-exact vs ffmpeg | **implemented, NOT verified — 2 of 3 root-cause bugs FIXED, 1 remaining, precisely localized** | commits `20f7d742`, `072f842f`; single-MB 16x16 black fixture decodes BYTE-EXACT vs ffmpeg; multi-MB witnesses still RED (decision 2563, see below) |
| M3 inter frames | **not attempted** (fixtures + spec recon done) | `clip-obs-320x192.ivf` (169 fr, 4 KFs), `altref-160x96.ivf`, `mparts-160x96.ivf` (8 partitions), `gop-160x96.ivf`; §16-18 read; inter trees/tables in `modes.rs` |
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
- M2: `cargo test -p ec-vp8 --test keyframe_exact` — ffmpeg byte-compare
  (`ffmpeg -v error -i F -f rawvideo -pix_fmt yuv420p -`) on 6 kf fixtures
  + mparts + WebP still. **FAILING — 0/3** (see below).

## Known gap (the M2 blocker) — TWO of three root causes FIXED this session

Method: a dixie-faithful python model of the whole partition-0 + token
parse (`/tmp/vp8ref.py`, decision-for-decision trace) diffed against the
Rust decoder's `EC_VP8_TRACE` decision stream.

1. FIXED — MV probability updates were parsed on KEY frames (dixie
   parses them only for interframes): 38 extra bool reads desynced
   partition 0. Moved inside `!is_keyframe`.
2. FIXED — decode_keyframe created a FRESH bool decoder over partition
   0 for the mode records (restarting the stream); `FrameHeader::parse`
   now returns its partition-0 decoder and the mode records CONTINUE
   from where the header stopped.
3. FIXED (earlier sweep) — per-MB-row token `reset_left` was never called.

Result: `kf-16x16-black-q0.ivf` (single MB) decodes byte-exact vs
ffmpeg. Multi-MB fixtures still diverge; the decision traces now match
to decision 2562 and split at 2563 inside the token walk (MB(0,1)
block 0): the reference consults probability 11, rust 4, at the same
stream position — a coefficient-context/bookkeeping difference that
only manifests from the second MB onward (cross-MB token contexts or a
block-sequencing slip). Debug aids: `EC_VP8_TRACE` (decision + node
trace), `EC_VP8_DEBUG` (per-MB coeff dump), `examples/dbg_first.rs`,
python model preserved at `scripts/vp8ref_model.py` (needs the RFC at
~/.cache/vp8/rfc6386.txt and tables.rs; run from the worktree root).
Additional finding: the divergent decision 2563 is ~28 reads into
MB(0,0) block 0's token walk (not MB(0,1) as first thought); both
sides read the same (pos,bit_count) stream position but consult
different table entries - the next step is to print the consulted
PROBABILITY on each G-annotation line on both sides and take the first
G-line whose prob differs (the rust G lines print n/band/ctx/prob;
the python model needs the same one-line prob added to its G prints).

## Repro

```
CARGO_TARGET_DIR=~/.cache/cargo-target-vp8 cargo test -p ec-vp8
scripts/gen_vp8_fixtures.sh          # fixtures/vp8/*.ivf (gitignored)
```
