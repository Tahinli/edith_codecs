# lane-b128res — the parked 128 residual arm after edge128

Base: `6de54e81` (lane-b128res worktree). Disposition: **stays-off**.
`EC_AV1_B128RES` keeps its default (off). Pins untouched at 8291 / 33227
(no default change, so no pin run). Never merged, never pushed.

## 1. The 8-sample mismatch: GONE

lane-edge128's diagnosis was right: `read_cdef` codes ONE literal for the
block that first reaches a unit and copies it over every unit the block
spans, while the encoder's CDEF search priced a preset per 64x64 unit —
`crate::tile::cdef_unit_owner` (tile.rs) collapses the four to the one the
decoder derives. On this HEAD the parked recipe re-measures clean:

- Command: `systemd-run --user -p MemoryMax=14G --wait --unit=b128res-probe
  bash -lc 'cd <worktree> && CARGO_TARGET_DIR=$HOME/.cache/cargo-target-b128res
  EC_AV1_B128RES=1 cargo test -p ec-av1 --release --lib --
  encode::tests::b128res_mismatch_probe --ignored --exact --nocapture'`
  (throwaway probe, removed after the run; recipe = the gate's own:
  bars 2160p fixture, `gate_crop` window 1920x1024:960:568, 12 frames,
  `encode_sequence_with_ctx(…, 90, …)`).
- frame 11 q=90, luma rows 126..127 cols 1611..1616 (idx 243531..243536,
  245451..245456): encoder reconstruction == ffmpeg == decode_stream, all
  eight samples read 107. Whole stream (12 frames, Y/U/V): three-way EXACT.
- Broader: with `EC_AV1_B128RES=1` BOTH BD gates passed their standing
  three-way exactness assertion (encoder == ffmpeg == decode_stream) at
  every clip x every q point x every frame — 5 clips x 4 q x 12 frames and
  2 films x 4 q x 48 frames.

EVIDENCE: ~/.cache/b128res/probe-mismatch.log | EC_AV1_B128RES=1, q=90,
frame 11, rows 126..127 cols 1611..1616 + whole-stream sweep | 8/8 samples
agree (all 107); 0 differing samples in any plane/frame; "whole stream
three-way EXACT". LOADAVG 1.96/2.43/3.42 before, 2.44/2.51/3.41 after.

## 2. Long-GOP BD (deciding; 48 pictures, gop=48, both films)

Control = default (off), arm = `EC_AV1_B128RES=1`, same HEAD, same ladder
(`encode::tests::bd_rate_film_long_gop`, `--ignored --exact`). BD-rate,
lower better:

| row | off vs libaom | off vs rav1e | on vs libaom | on vs rav1e |
|---|---|---|---|---|
| film A (1080p) 1920x768 | +20.9% | -9.0% | +20.8% | -9.1% |
| film B (2160p HDR) 1920x1024 | +70.4% | -2.2% | +70.5% | -2.1% |

Wall ours: film A 542.3s -> 549.8s (+1.4%), film B 422.9s -> 445.5s (+5.3%).
Control reproduces the standing table (film B +70.4/-2.2) to the digit.

EVIDENCE: ~/.cache/b128res/longgop-control.log | gate 2a, MemoryMax=14G |
film A +20.9%/-9.0%, film B +70.4%/-2.2%. LOADAVG 2.64/2.57/3.07 before,
3.44/2.79/2.76 after.
EVIDENCE: ~/.cache/b128res/longgop-arm.log | gate 2b, EC_AV1_B128RES=1,
MemoryMax=14G | film A +20.8%/-9.1%, film B +70.5%/-2.1%. LOADAVG
3.35/2.80/2.76 before, 3.07/3.53/3.19 after.

## 3. 12-frame native guard (all five rows)

`encode::tests::bd_rate_screen_native`, control and arm:

| row | off vs libaom | off vs rav1e | on vs libaom | on vs rav1e | wall ours off->on |
|---|---|---|---|---|---|
| bars 1080p | -3.3% | -19.0% | -3.4% | -18.9% | 160.4s -> 164.4s |
| bars 2160p | +8.4% | -13.9% | +8.4% | -13.9% | 137.1s -> 143.7s |
| film A | +17.9% | -6.3% | +17.8% | -6.4% | 144.2s -> 145.0s |
| film B | +22.6% | -3.8% | +22.6% | -3.8% | 122.8s -> 123.8s |
| screen capture | +14.4% | -33.2% | +14.5% | -33.2% | 94.9s -> 94.3s |

EVIDENCE: ~/.cache/b128res/native12-control.log | gate 3a, five rows,
MemoryMax=14G | table above, "off" columns. LOADAVG 2.38/3.32/3.13 before,
3.63/3.57/3.34 after.
EVIDENCE: ~/.cache/b128res/native12-arm.log | gate 3b, five rows,
EC_AV1_B128RES=1, MemoryMax=14G | table above, "on" columns; three-way
sample exactness held at every point. LOADAVG 3.42/3.53/3.33 before,
3.32/3.23/3.27 after.

## 4. Residual fire counts (writer `take_sb128_residual_hits`)

Per clip, cumulative over the run's four-q ladder (12-frame rows: 48
encoded frames; long-GOP rows: 192):

| clip (frames encoded) | search won | writer coded | per frame |
|---|---|---|---|
| bars 1080p (48) | 1567 | 63 | 1.3 |
| bars 2160p (48) | 985 | 32 | 0.7 |
| film A 12f (48) | 2331 | 92 | 1.9 |
| film B 12f (48) | 1797 | 47 | 1.0 |
| screen capture (48) | 307 | 61 | 1.3 |
| film A long-GOP (192) | 10205 | 320 | 1.7 |
| film B long-GOP (192) | 6056 | 75 | 0.4 |

0.4-1.9 fires/frame against thousands of whole 128 roots — still INERT,
exactly the historical 0-2 band. The census is now a permanent line of the
native arm ("128x128 roots … with a residual … (search) / … (writer)"), so
the next reader does not need a custom run.

## 5. Keep rule — NOT met

One film row >=0.5 BD down on a column: best delta anywhere is -0.1
(film A long-GOP and 12-frame). Other film row flat within +/-0.3: yes
(+0.1). Screen not worse by 0.3: yes (+0.1). Wall <= +15%: yes (max +5.3%).
The deciding column fails, and the arm is inert. Inert + exact = the
charter's stays-off branch.

## 6. Witnesses (both `--ignored`, run standalone)

- `a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact`:
  PASS as-is — writer coded 566 residual 128 blocks, 24 CDEF units taking
  another unit's literal, byte-exact through both decoders. This is the
  witness lane-edge128 built for the exact interaction that used to break
  bars 2160p.
EVIDENCE: ~/.cache/b128res/witness-cdef.log | --ignored --exact run |
"writer coded 566; covered 24", ok. LOADAVG 2.03/2.21/3.13 before,
2.72/2.36/3.15 after.
- `a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`:
  was RED on its precondition at its fixture q (150: writer coded 0) and
  still at 90 — the forced residual arm replaces the skip arm's cost
  unconditionally, and the cost model moved under it since lane-b128r
  (lane-rectdq's delta_q group on every non-skipped root; the pyramid's
  per-level q), so the NONE root lost its partition at all 420 roots and
  nothing reached the writer. Fixture re-vitalized at q=60: writer coded
  52, byte-exact through both decoders, PASS. q change + a doc paragraph
  naming the drift class are in the commit; the `coded > 0` assert stays
  as the guard.
EVIDENCE: ~/.cache/b128res/witness-residual.log (q=90, FAILED, writer 0),
witness-residual-q60.log (q=60, "search won 420, writer coded 52", ok).

## 7. Changes on the lane

- encode.rs `b128_residual` doc: the "open filter bug" paragraph is
  replaced by the closed story (owner map, re-measured three-way EXACT)
  and the inertness numbers; the knob stays as the arm's gate.
- encode.rs `native_bd_arm`: one census line added — residual fires
  (search/writer) next to the standing 128-root census.
- encode.rs pan residual witness: fixture q 150 -> 60 + drift note.
- Throwaway probe removed after its run.

## 8. Non-goals respected

Rect128/Ab128 residual pieces (a rect half carrying a residual still
refuses in `write_inter_block_128_rect`, owner map still Whole128-only),
merging, pushing, full workspace suite (default did not flip). Media files
read-only; all runs gated under `systemd-run --user -p MemoryMax=14G`, one
gate at a time, `--ignored --exact` on the ignored tests, logs under
`~/.cache/b128res/`.
