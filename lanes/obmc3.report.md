# lane-obmc3 — OBMC on the real film rows, the interp filter, the reference set

Worktree `edith_codecs-obmc3`, branch `lane-obmc3` off main 19668acf. Every arm
is the prebuilt release lib-test binary running `bd_rate_screen_native` (12
pictures, `gop 12`, four quantizers) or `bd_rate_film_long_gop` (48 pictures,
`gop 48`), each row read off that log's own header line.

## 0. The controls reproduce (charter said to check)

| clip | control vs libaom / vs rav1e | charter |
|---|---|---|
| film A, 12 frames | +21.7 / -4.4 | +21.7 / -4.4 |
| film B, 12 frames | +26.9 / -0.6 | +26.9 / -0.6 |
| screen, 12 frames | +20.1 / -30.4 | +20.8 / -30.1 (STALE by 0.7/0.3) |
| film B, long GOP | +89.8 / +9.1 | +89.8 / +9.1 |

Three of the four land to the digit; the charter's screen number is stale.

## 1. OBMC on the 12-frame native gate — REJECTED

`EC_AV1_OBMC=1`, `EC_AV1_OBMC_MIN` as named; the fire column is the log's own
`motion_mode ... % OBMC of N eligible` line.

| arm | film A | film B | screen | OBMC fire A / B |
|---|---|---|---|---|
| off (control) | +21.7 / -4.4 | +26.9 / -0.6 | +20.1 / -30.4 | 0% / 0% |
| min side 8 | +21.7 / -4.3 | **+27.2 / -0.3** | +20.0 / -30.4 | 15.5% / 9.7% |
| min side 16 | **+21.5 / -4.5** | **+27.6 / +0.1** | +20.1 / -30.4 | 10.0% / 6.9% |

min 8 is 0.3 worse on BOTH film B columns and 0.1 worse on film A's rav1e
column; min 16 buys film A 0.2/0.1 and costs film B 0.7/0.7. The screen row is
flat (+/-0.1) at both sizes, so the screen gate is not the obstacle — the film
rows are. Walls are within the parallel-arm noise (film A 193.5s control vs
195.7s min 8 vs 196.7s min 16, film B 144.4 / 145.8 / 146.2s), i.e. OBMC costs
~1% of encode wall on this gate.

## 2. OBMC on the long-GOP gate (film B, 48 pictures) — a 0.1-0.2 gain, still not a keep

| arm | film B | OBMC fire | wall ours:libaom:rav1e |
|---|---|---|---|
| off (control) | +89.8 / +9.1 | 0.0% of 238881 eligible | 477.5s:79.0s:61.9s |
| min side 8 | +89.6 / +9.0 | 10.7% of 241966 | 514.1s:96.1s:55.5s |

The two gates disagree in SIGN on film B: 0.3/0.3 worse over 12 pictures,
0.2/0.1 better over 48. A 0.1-0.2 move is far under the keep rule (both films
down on both columns, or one down >=0.5 with the other flat) and it costs
+7.7% wall. **Decision: OBMC stays OFF at every preset**, `speed.rs` untouched.
This is now the sixth refutation and the first one taken on real film content
rather than the colour-bars fixtures.

## 3. The interp filter — the charter's premise is refuted by libaom's own stream

Ours: `encode.rs` writes `interpolation_filter: Eighttap` in every frame
header, so no block codes an `interp_filter` symbol and every prediction runs
the REGULAR kernel (`Some(mc::InterpFilterKind::Regular)` at the search, trial
and recon call sites).

libaom: the long-GOP film B window re-encoded exactly as `external_ladder`
does it (`-cpu-used 6 -b:v 0 -crf 41 -g 48 -threads 1 -f obu`, 45941 B against
`lanes/libcen.report.md`'s matched 45938 B) and read back with
`EC_AV1_BITCENSUS=1 syntax_census` codes

    switchable_interp 0/0

in EVERY level row (key, ARFs, leaves) — libaom picks a fixed frame filter on
this content too. There are ZERO per-block interp-filter bits at stake on film
B, so the per-block switchable search was NOT built (ladder rung 1: it does not
need to exist). The finding is recorded next to the header field in
`encode.rs`. Open follow-up, if anyone wants it: libaom chooses WHICH fixed
filter per frame; we always send REGULAR. That is a frame-level knob, not a
per-block search, and it was not measured here.

## 4. The reference set — deferred, with the reason

`encoder.rs:1663` maps `ref_frame_idx` as `[LAST, LAST2, LAST3, GOLDEN, ...] =
[last_slot, last_slot, last_slot, GOLDEN_SLOT, last_slot, last_slot,
altref_slot]`: LAST2/LAST3 name the SAME DPB slot as LAST, so a second past
reference is not merely unoffered by the search, it is not retained. A
best-of-{LAST, LAST2} SAD census therefore needs the slot bookkeeping built
first (a second retained past slot + the search offering it + a witness), which
is feature work, not instrumentation. Deferred; unblocked by a lane that owns
`encoder.rs`'s DPB slots.

## 5. What ships

Nothing but the measurements: two doc blocks in `crates/ec-av1/src/encode.rs`
(the OBMC table at `obmc_min_side`, the interp census at the header field) and
this report. No default moves, no pin is re-pinned (8562 / 33357 stay OFF), no
`speed.rs` edit.

## 6. Invariants

Diff is doc comments only — no code path moves, so the shipped bytes are
unchanged by construction.

* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1
  warnings (the 21 ec-opus + 1 ec-vorbis warnings pre-date this lane).
* `cargo test -p ec-av1 --release --lib` (detached): **569 passed; 0 failed;
  45 ignored**, 858.8s — the expected totals exactly.
* Pins 8562 / 33357 stay OFF and were not re-pinned (no arm passed).
