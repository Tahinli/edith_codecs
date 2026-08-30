# lane-realworld r5 — finish delta_q / delta_lf

At dbfd67c — r4's work (cdf.rs, cdf_state.rs, decode.rs), committed verbatim by
the orchestrator at its cap and **never seen to compile**.
`lanes/realworld-r4.charter.md` is still binding; `lanes/realworld-r3.report.md`
is the verified typing plan. Do not re-derive either.

1. `cargo check`, fix whatever r4 left mid-edit, then the full suite. Getting
   dbfd67c green is the whole job until it is green. COMMIT.
2. Finish the quantizer half: the `CURRENT_Q_IDX` thread-local reset per tile
   and read at the 2 real dequant sites (r3 proved `base_q_idx` is passed
   through ~90 sites but read at only 2). Refuse the loop-filter half by name in
   between if it is not ready. COMMIT.
3. The `DeltaLF` -> deblocker path: `MiGrid` / `fill_lf_grid` needs a new
   per-block field. This is the one genuinely new design piece. COMMIT.
4. Remove the stream.rs refusal and gate it: `EC_AV1_REQUIRE_AOMENC=1`,
   `-t <seconds>` on the ffmpeg generate, fixture through `gradients_source`,
   aomenc `--threads=1 --row-mt=0 --sb-size=64` plus `--deltaq-mode=`/`--aq-mode`,
   MORE THAN ONE SUPERBLOCK (a per-superblock symbol cannot fire otherwise), and
   a HARD-asserted thread-local firing count.

Reminders that cost other lanes a round: the CDF counter reset must cover the
new tables (class `cdf-counter-not-reset`); do not put a refusal in front of the
code you just added and then claim it runs (class
`refusal-short-circuits-its-own-code`) — prove the new read fires with a counter
while the frame still refuses; and main now FAILS the suite if a gate turns a
decode error into a printed SKIP, so write the gate as an attempt loop that
requires at least one decode.

Merge note: main is at 91a08e8 with `gate_coverage.rs` and
`refusal_inventory.rs`. Report every refusal string you add or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. End with `lanes/realworld-r5.report.md`, VERDICT on line 1.
