# lane-av1-gwarp12 — the single-ref GLOBAL-warp witness at 12 bits

Base fe3e8418, worktree `edith_codecs-av1gwarp12`, branch `lane-av1-gwarp12`.
The charter: close lane-av112bitw's open half — "single-ref GLOBAL-warp at 12
bits has no stream of its own" — with a real `aomenc --bit-depth=12` stream
whose single-reference inter blocks are predicted through
`warp::global_warp_params` under a ROTZOOM model, decoded against BOTH pixel
oracles, or an honest miss table.

## Result: FOUND, first recipe, pixel-exact against both oracles

Encode attempts aimed at the target: **2** (budget 8). Attempt 1 (the gate
itself) fired and was already byte-exact vs ffmpeg; attempt 2 re-encoded the
same recipe standalone for the aomdec oracle.

1. `aomdec == ffmpeg` on the probe stream
   (`/tmp/gwarp12probe/probe.obu`, sha256
   `3c6102cd0dd9bbccb5d15b2f3605a7dcea9e3189c1efc7d0317b16fc481af5fe`,
   37194 bytes; 16x 128x128 frames, 786432 raw bytes each decoder,
   `cmp`-equal payloads). Since the gate proves our decode == ffmpeg on the
   live re-encode, all three decoders agree on the firing stream.
2. Engagement: `rotzoom_gm_warp_hits = 152` (16x16+ single-ref blocks
   predicted through `global_warp_params` with a ROTZOOM model), affine 0
   (vanilla aomenc cannot emit it, see below), local-warp symbol 0
   (`--enable-warped-motion=0` isolates the global arm),
   `compound_warp_hits` delta 0 (hard-asserted; single-ref premise held).

## Why the recipe works when the prior ones didn't

Every prior 12-bit content fit TRANSLATION global models (excluded by
`is_global_mv_block`) or reached >TRANSLATION models only on compound blocks.
The lever is composition, not the encoder:

- **Content**: rotation + UNIFORM zoom — a similarity transform, exactly the
  family the 4-parameter ROTZOOM model expresses (`mandelbrot` +
  `rotate=a=0.12*t` + symmetric `scale` over `n`, cropped 128x128).
- **Compound tools OFF** (`--enable-masked-comp=0 --enable-interintra-comp=0
  --enable-onesided-comp=0`, plus lag 0 / alt-ref 0 so all refs are
  forward): a global model can then only ride a single-reference GLOBALMV
  block.
- **`--enable-warped-motion=0`**: no local WARPED_CAUSAL can take
  `warp_params` first; the `warp_params.is_none()` guard at the single-ref
  arm makes every hit unambiguously a global-warp hit.
- **`--min/--max-partition-size=32`**: every inter leaf is 16x16+, so the
  8x8-leaf arm (`decode_inter_block8`, leaf8 lane's file region) is
  structurally out of the stream and my counter proves MY site, not a twin.
- **Model bound is encoder-side**: `global_motion_facade.c:24` pins the gm
  search to ROTZOOM, so every >TRANSLATION single-ref model a real aomenc
  can emit IS ROTZOOM. (AFFINE needs the widened side build and has its own
  gate + `affine_gm_hits` at 8/10 bits.)

Exact flags: the `encode_12bit` witness base (cq 20, cpu-used 0, sb 64,
profile 2, cdef/restoration ON) plus the extras above and
`--enable-global-motion=1 --enable-obmc=0 --enable-ref-frame-mvs=0`.
Fixture: `y4m_12bit_rotzoom_gm_source` (ffmpeg lavfi, C420p12 y4m, 16
frames). The gate re-encodes live; engagement reproduced identically across
three runs (152/152/152).

## What changed

- **decode.rs** — one new counter pair at the single-ref global-warp arm
  (`decode_inter_block`, the lane's own site): `ROTZOOM_GM_WARP_HITS` +
  `rotzoom_gm_warp_hits()`, bumped when `global_warp_params` resolves AND
  `gm_ref.model == Rotzoom`, mirroring the `AFFINE_GM_HITS` twin. The 8x8
  leaf arm's copy was deliberately NOT touched (leaf8 lane).
- **stream.rs** — `y4m_12bit_rotzoom_gm_source` fixture helper + the gate
  `a_real_rotzoom_global_warp_12bit_stream_decodes_pixel_exact`:
  `assert_12bit_sequence_header` preflight, hard `delta_rotzoom > 0` (a
  warpless stream cannot pass), hard `compound_warp_hits` delta == 0, and
  every plane of every frame byte-equal vs ffmpeg
  (`ffmpeg_decode_sequence_12bit`).

No decoder arithmetic changed: the single-ref path was lifted by
lane-av112bitw (parameterised `warp_round_0`) and the composition argument
held — the stream proves it instead of arguing it. Non-vacuity is by
construction (counter assert), and the gate FAILED-category risk (silent
skip) is closed: encode/ffmpeg presence is hard-asserted inside the gate.

## Verification

- New gate green (encode attempt 1): 16 frames pixel-exact vs ffmpeg,
  152 rotzoom hits.
- aomdec oracle probe (encode attempt 2): aomdec == ffmpeg, sha256 above.
- Scoped sweep of the 12-bit family the counter addition touches: 9/9 green
  (`12bit` filter) including `a_12bit_warped_motion_stream_decodes_pixel_exact`
  (engagement still 7/1/0), both compound global-warp 12-bit gates (16x16+
  and 8x8 leaf), grain refusal, screen refusal.
- `cargo check -p ec-av1 --all-targets`: clean, no warnings.

## Create-list audit

One new file: this report. Touched: `decode.rs` (counter), `stream.rs`
(helper + gate). No fixture committed (gate encodes live), no junk files;
probe artifacts under /tmp are throwaway.
