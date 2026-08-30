# lane-realworld r4 — delta_q / delta_lf

At the lane tip (main merged in). Read `lanes/realworld-r3.report.md` — it is a
typing plan, verified against the real libaom source. Do NOT re-derive it
(class `worker-cap-spent-reading`). Job 1 (the part64 silent desync) is CLOSED
and merged to main.

## What r3 verified for you
- Default CDFs, from `~/.cache/aom-oracle/src/av1/common/entropymode.c:840-852`:
  `DELTA_Q`, `DELTA_LF` and `DELTA_LF_MULTI[4]` are all
  `[28160, 32120, 32677, 32768, 0]`. `FRAME_LF_COUNT = 4`,
  `MAX_LOOP_FILTER = 63`, `DELTA_Q_SMALL = DELTA_LF_SMALL = 3`.
- The read gate collapses to a per-superblock `Cell<bool>` exactly like
  `CDEF_TRANSMITTED` (decode.rs:82) — libaom's `b_row == 0 && b_col == 0` check
  is always true for a partition tree's first-decoded leaf.
- Only FOUR call sites need the new read, the same four `maybe_read_cdef_idx`
  already uses: `read_intra_mode_rect`, `read_intra_mode`, `decode_inter_block`,
  `decode_inter_block8` (line numbers have shifted since r3 — find them by the
  `maybe_read_cdef_idx` calls).
- `base_q_idx: u8` is passed through ~90 sites but READ at only 2, both
  `dequant_and_inverse_typed` calls. A `CURRENT_Q_IDX` thread-local reset per
  tile and read at those 2 sites reproduces AV1's running quantizer without
  threading a parameter through every signature.
- Open design question, the one genuinely new piece: `DeltaLF` reaching the
  deblocker (`lf_level` / `edge_params`) has no per-block carrier —
  `MiGrid` / `fill_lf_grid` needs a new field. Do the quantizer half first and
  commit it; the loop-filter half can refuse by name in between.

## CDF wiring is four sites
Struct field, defaults array, save/restore, per-frame counter reset. In this
codebase `reset2`/`reset3` are length-generic and save/restore is a whole-struct
Clone, so the defaults array is what needs hand-checking — but VERIFY the
counter reset covers the new tables (class `cdf-counter-not-reset`: a missing
reset gives right values at the wrong adaptation rate).

## Then the gate
Remove the stream.rs refusal ("a frame with delta_q_present or delta_lf_present
set"), and gate it: `EC_AV1_REQUIRE_AOMENC=1`, `-t <seconds>` on the ffmpeg
generate, fixture through `gradients_source`, aomenc `--threads=1 --row-mt=0
--sb-size=64` (this decoder hardcodes 64px superblocks) plus whatever turns
delta_q on (`--deltaq-mode=`, and `--aq-mode`); the fixture needs MORE THAN ONE
SUPERBLOCK or a per-superblock symbol can never fire. HARD-assert a thread-local
firing count.

Merge note: main is at 53b319b with `gate_coverage.rs` and
`refusal_inventory.rs`. Report every refusal string you add or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. End with `lanes/realworld-r4.report.md`, VERDICT on line 1.
