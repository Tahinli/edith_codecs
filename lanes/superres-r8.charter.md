# lane-superres r8 — inter-frame superres

At 8346976, MERGED into main (3019019). Key-frame superres is closed: 3/3 frames
pixel-exact, `superres_hits=3`. Read `lanes/superres-r7.report.md`.

Start with `git merge main` (main is a8724cb — tile rows, delta_q/delta_lf,
palette-Y, a `bit_depth != 8` refusal, three guard tests), resolve, suite green,
commit.

## The round
`decode_stream` refuses `"an inter frame with use_superres set (this decoder
never scales its motion-compensated reference to match the current frame's
downscaled size, spec 7.11.3.3)"`. Implement it.

The real work is scaled-reference motion compensation: when a reference frame's
dimensions differ from the current frame's downscaled size, the predictor is
sampled with a per-block step derived from the scale factors, not a plain copy.
libaom's `av1_setup_scale_factors_for_frame` and `av1_convolve_2d_scale` are the
reference; spec 7.11.3.3 gives the same in `xStep`/`yStep` terms. Expect the
filter selection and the subpel precision to differ from the unscaled path.

Order, each committed:
1. A gate FIRST that produces a real inter frame with `use_superres`.
   `--superres-denominator` controls NON-key frames (`--superres-kf-denominator`
   is the key-frame one — this lane learned that the hard way), so the recipe is
   within reach. Drive the tile decode BELOW `decode_stream` so the refusal
   cannot short-circuit the code you are measuring
   ([[refusal-short-circuits-its-own-code]]).
2. Then the scaled MC path, bisected against the oracle with a range/pixel
   ladder rather than by inspection.
3. Then, and only with the gate pixel-exact, lift the refusal and update
   `refusal_inventory.rs` in the same commit.

Instrument, do not hand-trace: r6 burned a round hand-tracing what r7 closed in
one look at libaom's `set_lpf_parameters` early return. Rung numbers 6, 7, 8, 8b
are taken in `scripts/instrument-aom-oracle.sh` on main — take 9. The oracle at
`~/.cache/aom-oracle` is SHARED with five sibling lanes: env-gated rungs only,
silent when unset, idempotent, never a throwaway patch left in the tree.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
into main. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/superres-r8.report.md`, VERDICT on line 1.
