# lane-rectres — non-skip residual on Rect128/Ab128 pieces

GOAL: the Whole128 residual arm (`EC_AV1_B128RES`) is exact but INERT
(0.4-1.9 writer fires/frame, re-measured lane-b128res on 3b4673fe) —
whole 128 roots almost never win RD. The 128 root's rectangular pieces
DO win: lane-b128hv ships HORZ/VERT skip halves and lane-ab128 ships the
four AB shapes (`EC_AV1_B128AB=1` default ON). But those pieces are
skip-only: `write_inter_block_128_rect` refuses a rect half carrying a
residual, and `crate::tile::cdef_unit_owner` (tile.rs) is Whole128-only.
This lane turns Rect128 and Ab128 pieces into residual carriers and
measures whether RD taking them moves BD.

Work ONLY in this worktree (`lane-rectres`,
`/home/tahinli/Documents/Code/Rust/edith_codecs-rectres`). Absolute paths
only. Never touch main's checkout, never push, never `git checkout
<file>`, never repo-wide `cargo fmt`. `export
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rectres` on every cargo
command. Logs under `~/.cache/rectres/`. TMPDIR only on gate command
lines, not on the sccache server. Gates: `systemd-run --user -p
MemoryMax=14G`, `--ignored --exact` on both BD tests (they are
`#[ignore]`; without `--ignored` they measure nothing). One gate at a
time; record LOADAVG before AND after. Pins 8291 / 33227. Keep rule: one
film row ≥0.5 BD down on a column, the other flat within ±0.3, screen
not worse by 0.3, wall ≤+15%. Sign: BD-rate, lower better.

## 1. Write residual on rect/AB pieces

- Lift the refusal in `write_inter_block_128_rect`: a Rect128 half
  (HORZ/VERT) or an Ab128 shape piece with a real residual codes its
  coefficients like any inter block of that w×h. The decoder side is
  spec — `decode_stream` and ffmpeg both already read residual on any
  inter partition shape; the burden is on the ENCODER writer and the
  encoder reconstruction path being identical.
- Knob in the `EC_AV1_*` convention (e.g. `EC_AV1_RECTRES`), default
  OFF, gated next to `b128_residual` in encode.rs. OFF = today's
  behavior bit-exact.
- INVARIANT (lane-edge128's bug class): a 128-wide piece that writes a
  residual also codes `cdef_idx` per the syntax it carries, while
  `read_cdef` copies ONE literal over every 64x64 unit the piece spans.
  Extend `cdef_unit_owner` past Whole128 to the rect/AB shapes so the
  CDEF search prices the ONE index the syntax carries, not one per unit.
  Skip-only pieces still write no `cdef_idx` — owner map must not fire
  for them.
- RD side: let the rect/AB search arm price non-skip outcomes (residual
  cost) instead of skip-only, under the knob.

## 2. Witnesses FIRST (before any BD gate)

Add witnesses in the standing pattern (see
`a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`
and `a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact`,
both `--ignored`, fixture q=60 after lane-b128res re-vitalization):

- a Rect128 half with a real residual decodes sample-exact through
  ffmpeg AND `decode_stream`, with a `coded > 0` assert as the guard.
- same for at least one Ab128 shape.
- a rect/AB residual piece spanning multiple CDEF units takes the owner
  unit's literal, byte-exact through both decoders (the edge128
  interaction, now on rect shapes).

If any witness shows an encoder-reconstruction mismatch: STOP, do not
ship, report class + path:line, default stays OFF.

## 3. Measure

Control = default (knob off) on this HEAD. Arm = knob on.

- `cargo test -p ec-av1 --release --lib -- --ignored --exact --nocapture
  encode::tests::bd_rate_film_long_gop` (deciding). Both films.
- 12-frame guard: `encode::tests::bd_rate_screen_native`, all five rows,
  control and arm.
- Fire census: extend the standing native-arm census line so rect/AB
  residual fires (search won / writer coded) print next to the 128-root
  census, and report fires/frame per clip like lane-b128res §4. If the
  writer still codes ~0 rect/AB residuals, the arm is inert — say so.

Keep rule met → default ON, drop the refusal comment. Inert + exact →
leave OFF, document fires. Mismatch → OFF, report (see §2).

## 4. Pins / suite (only if you change the default)

Pins at default and `EC_AV1_SPEED=6`. Workspace check `timeout 900 cargo
check --workspace --all-targets -j4` (0 ec-av1 warnings). Do NOT run the
full lib suite unless you change the default; if you do, three lanes:
`--skip stream::` / `stream:: --skip 10bit` / `10bit`.

## DONE

Commit on `lane-rectres` + `lanes/rectres.report.md` with EVIDENCE lines
(`EVIDENCE: <artifact> | <steps> | <measurement>`), fire counts, BD
tables, witness results, disposition `ships-on | stays-off | stopped`.
Never merge or push.
