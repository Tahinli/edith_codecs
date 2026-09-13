# lane-mp3-perf — ec-mp3 encode speed vs the incumbent MP3 encoder 0.6.1 (r1)

Charter: close the ec-mp3 ENCODE gap (the seat went out at ~7-8x realtime stereo
CBR against the incumbent's 147-283x single-thread; a 2 h film's stereo track
cost ~17 min of export wall). Branch lane-mp3-perf off 0d81d6c5.
Worktree-only; no merges, no pushes.

## Method

- A/B harness: `~/Documents/Code/Rust/mp3-perf-ab` (scratch crate outside the
  family workspace, opus-perf precedent — it links the crates.io incumbent
  0.6.1 beside ec-mp3 by path into this worktree; the workspace shim carries
  the same crate name so this cannot live inside).
- PCM: deterministic synthesized music (3 s stereo 44.1 k), a 1 kHz tone
  (3 s mono 48 k), 30 s of the 44.1 k stereo music the family gates use, and
  the crate's own mp3src fixtures — all cached as raw interleaved f32, both
  engines fed byte-identical bytes in-process.
- Protocol per row: one warm-up each side, 7 alternating rounds (A/B and B/A
  by round parity), min per side, /proc/loadavg per round. Single-threaded
  both sides (the incumbent is thread_local-only).
- Quality gate per commit: the crate's own release suite (21 lib + decode
  matrix + encode matrix) green, every fixture decoded through ffmpeg, and
  aligned correlation vs source compared to the pre-change build on the same
  PCM. Byte-identity of the encoded stream was the stronger check wherever it
  held (it held everywhere except where noted).

## Baseline (0d81d6c5, load 1.8-2.2)

| row | ec-mp3 | incumbent | ratio |
|---|---|---|---|
| music3s cbr192 | 239.0 ms / 12.6x | 23.0 ms / 130.6x | 0.096 |
| real30s cbr192 | 3661.2 ms / 8.2x | 190.1 ms / 157.8x | 0.052 |
| tone48k cbr192 mono | 106.3 ms / 28.2x | 7.8 ms / 385.6x | 0.073 |
| real30s vbr | 7540.3 ms / 4.0x | 189.5 ms / 158.3x | 0.025 |

(ec VBR = quality 0.5 on its own scale; the incumbent's VBR takes a target
mean kbit/s, so that row passes 190 on its side — roughly rate-matched.)

## Profile at baseline, and what each step did

Order of attack was measured, not guessed: 45% BitWriter per-bit loops, 22%
code_spectrum's Vec<bool> round-trip, 10% libm cos in the MDCT, ~12% powf/exp2.

| commit | change | stereo CBR | VBR | quality |
|---|---|---|---|---|
| 1fbe9240 | word-buffered BitBuf; rate loop prices gains by Huffman cost alone, materialises bits once | 18.2x / 0.115 | 10.6x | byte-identical, all 8 |
| c056aa4d | 3/4-power as cube+two sqrts; band gains hoisted per band | 21.1x / 0.134 | 13.0x | corr equal to 6 decimals on all 8; 7/8 byte-identical (music3s flips one ulp-boundary line) |
| f51fef8f | forward-MDCT cosine kernels tabulated (LazyLock f32, f64 accumulation kept) | 37.3x / 0.235 | 17.8x | byte-identical, all 8 |
| 891dd0b2 | best_table_cost: one walk over the region accumulates every candidate table | 43.7x / 0.278 | 20.9x | byte-identical, all 8 |
| 46a73869 | gain search brackets by doubling stride + bisection (was a one-step walk; instrumentation showed ~31.5 quantise+plan evaluations per granule) | 96.0x / 0.608 | 52.4x | 7/8 byte-identical; music3s as above |
| bdb12e0c | cursor-based push drain (was quadratic memmove), deinterleave scratch, no per-granule pending clone | 109.8x / 0.693 | 55.7x | byte-identical, all 8 |
| e51d830d | quantise_span: the level curve eight lanes wide through wide::f32x8 | 127.9x / 0.819 | 67.5x | byte-identical, all 8 |
| 0266ddcb | region walk: zero-pair fast path, per-candidate range feasibility upfront | 139.2x / 0.886 | 74.3x | byte-identical, all 8 (music3s inherited) |

## Final table (0266ddcb, local, loadavg 2.9-3.6, min-of-7 alternating)

| row | ec-mp3 | incumbent | ratio | baseline |
|---|---|---|---|---|
| real30s cbr192 | 214.5 ms / 139.9x | 189.0 ms / 158.7x | **0.881** | 0.052 |
| music3s cbr192 | 47.0 ms / 63.8x | 22.7 ms / 132.3x | 0.482 | 0.096 |
| real30s vbr | 403.7 ms / 74.3x | 189.2 ms / 158.5x | 0.469 | 0.025 |
| tone48k cbr192 mono | 38.0 ms / 79.0x | 7.8 ms / 382.8x | 0.206 | 0.073 |

Target ≥0.7 of the incumbent single-thread (105x+ RT stereo CBR): **met** on
the 30 s music row, 0.881; the 2 h-film export cost drops from ~17 min to
~2.5 min of wall. Quality holds: every fixture's decoded correlation equals
the pre-change build to six decimals (music3s-cbr192: 0.999998 on both; its
stream moved by ulp-boundary level flips when the gain search and the f64→
scalar-curve change landed, both gated above).

## What remains, honestly

- **VBR 0.469.** VBR re-codes the whole frame per bitrate candidate (bisection
  over the legal-rate table), so it pays the CBR cost several times. The same
  kernels are what it spends; a candidate-count reduction (start from the
  last settled index with a tighter bracket) is the next lever and is
  measurable with the existing harness.
- **Mono 0.206.** The incumbent's mono path is disproportionately fast
  (385x vs its 158x stereo); ours pays the same per-granule costs either way.
  Dense-content stereo — the seat the product pays — is where the work went.
- **Threading evaluated, declined.** Frame-level parallelism is constrained
  by the bit reservoir (a frame's budget depends on bytes actually consumed
  by predecessors) and the one-granule look-ahead; an API-compatible shape
  would pipeline one frame behind, and with the single-thread target met the
  added determinism surface buys nothing this round.
- Cross-machine final table on an idle VPS not run: both engines share one
  process here, so every ratio above is already same-machine/same-load; a
  quiet-box pass would tighten the absolute x-realtime numbers only.
