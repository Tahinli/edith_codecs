# lane ac3parity: AC-3 parity with ffmpeg on dense pink noise

Base: f5eb855b (main). Branch lane/ac3parity.

## Root cause

The residual is not a transform, coupling, rematrix or bit-allocation error. It is the
zero-bit mantissa dither (A/52 §7.3.4).

- Per-band analysis of the benchrows clip (channel 0, dither off): below 16 kHz the decoded
  bins match ffmpeg (coherence 1.0000 for 0-16 kHz). At 448 kb/s, bins from about 16 kHz up
  are zero-bit mantissas. ffmpeg fills them with noise (20-24 kHz at 31 dB, exactly where our
  dither-off output is at -52 dB).
- With dither on, our band power matches ffmpeg's to 0.01 dB, but coherence in the same bands
  is 0.34 (16-20 kHz) and 0.00 (20-24 kHz). Same level, different sequence.
- The spec does not define the dither sequence. The old generator was a 16-bit LCG, not
  ffmpeg's. Two decoders that both follow the spec therefore differ by exactly this noise.
- ffmpeg's float AC-3 decoder (libavcodec/ac3dec.c, master) uses `av_lfg_init(&dith_state, 0)`
  at decoder start (not reseeded per frame; `cons_noisegen` defaults to off). Each zero-bit
  coefficient draws `((x>>8)*181>>8) - 5931008` (units of 2^-23) from that lagged-Fibonacci
  generator. The coupling channel always draws; a coupled channel without dithflag then zeroes
  those bins. The draw order is channel by channel per block, with the coupling channel right
  after the first coupled channel.

## Changes

- `crates/ec-ac3/src/mantissa.rs`: `Dither` is now ffmpeg's AVLFG (state seed table checked
  against libavutil's `av_lfg_init` via ctypes; `av_lfg_get` as in lfg.h). `unit_variance`
  (E-AC-3 spectral-extension noise) uses the same generator, its own instance.
- `crates/ec-ac3/src/decode.rs`: the coupling channel draws dither for every zero-bit bin, once,
  when it is read. `decouple()` no longer redraws per coupled channel. It zeroes the bin when
  that channel has dithflag 0. Before, each coupled channel drew its own value.
- `crates/ec-ac3/src/lib.rs`: the docs now say the noise is ffmpeg's.
- `crates/ec-ac3/src/stack_budget.rs`: the inventory is updated for +520 bytes per decoder
  (`Ac3Decoder` 3184 -> 3704, `Core` 2864 -> 3384; budget 8192). Each `Dither` grew from 4 to
  264 bytes.
- `crates/ec-ac3/tests/ffmpeg_parity.rs` (new): the fixture generator. ffmpeg encodes six (or
  two) independent pink-noise sources at 10 s, then decodes with ffmpeg and with ec-ac3 (default
  options). Gate: per-channel correlation >= 0.999.

## Table: min per-channel correlation vs ffmpeg (10 s pink noise, default options)

| layout | bitrate | before (dither on, default) | before (dither off) | after (default) |
|---|---|---|---|---|
| 5.1 | 448k | 0.980963 | 0.990441 | 1.000000 |
| 2.0 | 192k | 0.981374 | 0.990647 | 1.000000 |
| 5.1 | 640k | 0.979751 | 0.989804 | 1.000000 |

"Before" figures come from the pre-change ac3dec binary on the 60 s benchrows clip for 448k
(and on identical ffmpeg encodes for the others). The 10 s test clips are regenerated in the
test itself.

Also seen before the change: coupling off (`-channel_coupling 0`) at 448k gave 0.974685 with
dither on. Rematrixing off at 2.0/192k gave 0.976133. Both are the same dither gap, not a
separate bug. They were not added to the test.

## Verify

Run with scripts/remote-run.sh ac3parity --host c. All on the server:

- `cargo test --release -p ec-ac3`: every test binary passes (lib 48 tests; integration
  binaries with 8, 7, 6, 3, 6 and 1 tests, the 3 being ffmpeg_parity; real_library skips with no
  library on the server). The ffmpeg_parity run prints the same 1.000000 numbers above.
- `cargo clippy -p ec-ac3 --all-targets -- -D warnings`: clean.
- `cd shims/oxideav-ac3 && cargo test --release`: 3 + 2 pass.

## Not done, and why

- DONE.md's ec-ac3 note says "Output is not bit-exact by design: dither differs from ffmpeg's".
  That is now false for AC-3 zero-bit dither. The orchestrator should update that line.
- E-AC-3 spectral-extension noise now uses the same generator type, but I did not check
  whether ffmpeg draws its spx noise from the same AVLFG. Spx is not in the parity test.
  E-AC-3 real-library and dependent tests still pass with dither off, so only the default dither
  output is affected.
- LFE zero-bit bins are still not dithered. The LFE in these streams has no zero-bit mantissas
  (corr 1.0). The question of whether ffmpeg dithers LFE is open.
- Real-library AC-3/E-AC-3 files were not re-run (no real_library.toml on the server).
