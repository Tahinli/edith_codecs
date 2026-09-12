# lane-opusperf — ec-opus speed A/B vs the FFT-built comparators (r1)

Charter: close the ~1.5x ec-opus speed gap vs ruopus 0.1.2 [std, spectrograms]
(decode) and opus-rs 0.1.26 (encode), the exact builds the old edith seats ran.
Branch lane-opusperf off d27a904d. Worktree-only; no merges, no pushes.

## Method

- A/B harness: `~/Documents/Code/Rust/opus-perf-ab` (scratch crate, outside the
  family workspace; ec-opus by path into THIS worktree). Also rsynced to
  VPS-3 (tCloud@178.105.165.182, `~/opusperf/`) with the media.
- Media: a real 5.1 film (`~/Videos/Films/Troy…1080P.AV1.OPUS.5.1-DECK.mkv`,
  read-only). `full51.opus` = the full 5.1 track remuxed by ffmpeg (clean);
  `film2.opus` = libopus 256k stereo transcode of the 600–1320 s window;
  `film2_seg.f32` / `film51_seg.f32` = 120 s of f32 PCM from the 600–720 s
  window. Both sides of every pair get byte-identical inputs.
- Protocol per row: one warm-up pass each side, then 7 alternating rounds
  (A/B and B/A by round), min per side, `/proc/loadavg` captured per round.
  Cross-check before the decode rows: both 5.1 decoders over 25 packets,
  worst-channel correlation > 0.999, final_range equality asserted.
- Profiles: perf record/annotate on a `prof` driver looping one workload.

## Baseline (d27a904d, local, loadavg 3.0–3.4)

| row | ec-opus | comparator | ratio |
|---|---|---|---|
| decode 5.1 120 s film | 170–174x | 246–248x ruopus | 0.69–0.70 |
| decode stereo 120 s film | 346x | 524x ruopus | 0.66 |
| encode 128k CBR stereo | 261x | 438x opus-rs | 0.59 |
| encode 256k CBR stereo | 187x | 319x opus-rs | 0.59 |

(The campaign's local numbers, 246x vs 407x / 1.41–1.57x, were taken on a
quieter box; sibling lanes were running during this round. All comparisons
below are same-session pairs, so ratios stay comparable.)

## Profile (the assignment's FFT hypothesis is wrong for both paths)

- decode stereo: `quant_band` 65% — inside it `decode_pulses` 43%
  (pvq_urow row-stepping 19% + uprev 11%); FFT (Fft15::inverse +
  inverse_split) ~10%.
- encode 256k: `quant_band` ~60–64% — `alg_quant` ~51% (search + init +
  encode_pulses 14%); FFT+MDCT ~9%. `floorf` 3.6%, stereo_itheta's f64
  atan2 ~2.5%.

## Landed

### 88f22a66 — PVQ U-table (decode + encode)

`decode_pulses`/`encode_pulses` now read the reference non-custom build's
1272-entry flat `U` table (const-computed from the same recurrence in
wrapping arithmetic; layout `U(m,c)` at `PVQ_U_ROW[m]+c`, symmetric in
min(n,k)) instead of stepping `U` rows at runtime: O(N) lookups vs O(N·K)
arithmetic. Decoder/encoder `urow` scratch fields deleted; a lib test
cross-checks every stored entry against a fresh runtime row-stepping build.

Fidelity: full `cargo test -p ec-opus --release` green per commit (35 lib +
28 conformance: RFC 6716 vectors range-state-exact, ffmpeg/libopus
correlation gates, oracle rate table, roundtrips at every rate,
garbage-payload fuzz, speed floors). Zero warnings (`#![forbid(unsafe_code)]`
holds).

Committed state, local, loadavg 0.3–7 (two consistent batteries):

| row | ec-opus | comparator | ratio | baseline |
|---|---|---|---|---|
| decode 5.1 | 207x | 251–254x ruopus | 0.82 | 0.70 |
| decode stereo | 461–465x | 529–533x ruopus | 0.86–0.88 | 0.66 |
| encode 128k CBR | 275–276x | 429–431x opus-rs | 0.64 | 0.59 |
| encode 256k CBR | 204–205x | 316–317x opus-rs | 0.65 | 0.59 |
| encode 510k CBR | 185–187x | 201–202x opus-rs | 0.92 | n/a |
| encode 256k VBR | 204–206x | 315–317x opus-rs | 0.65 | n/a |
| encode 256k | 204–206x | 408–411x ruopus | 0.50 | n/a |
| encode 5.1 384k (absolute; no comparator has a multistream encoder) | 87x | — | — | n/a |

## Measured and reverted (both on top of 88f22a66, not committed)

- FOUR independent champion groups in the vector sweep (killing the serial
  compare chain): no change — 0.65 at 256k either way. The chain was not the
  bottleneck.
- Padded 8-lane sweep for ALL band sizes (pad `yy` = 1e17 sentinel): REGRESSED
  encode to 153x / 0.48 at 256k. Instrumented counts
  (2.24 M alg_quant calls, 398 M scalar lane-visits vs 12 M vector
  chunk-visits) made small bands look dominant, but each small-band scalar
  visit is cheap and an 8-lane sweep of a 4-sample band wastes lanes. Reverted
  (`git checkout`); the instrumentation came out with it.

## What remains

1. VPS-3 clean-machine A/B (media + harness already in `~/opusperf` there) for
   an uncontended final table; local numbers above carry sibling load.
2. decode 5.1: 0.82 vs the 0.9 target. The remaining decode cost is spread
   (PVQ walk now table-driven); candidates: `bits2pulses` (2.7% linear scan),
   `dec_uint` path, denorm/deemphasis loops.
3. encode 256k: 0.65 vs the 0.8 target. The comparator's search is rsqrt-form
   AVX2 (libopus 1.6 port); `#![forbid(unsafe_code)]` rules out intrinsics and
   `wide` has no rsqrt, so the lever is either a safe rsqrt approximation
   (bit-trick via `to_bits`, needs a wide/i32 bridge) or reducing searches
   (allocation shape). `stereo_itheta`'s f64 `atan2` (≈2.5%) → the crate's own
   `fast_atan2f` is a safe small win.
4. FFT/MDCT lever (~9–10% of each path): realfft behind a default feature or
   further kernel shaping — worth it only after 2–3.

Housekeeping: `opus-perf-ab` is a scratch crate outside the family tree;
`fixtures` symlink in the worktree root is gitignored as usual.
