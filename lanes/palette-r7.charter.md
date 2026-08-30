# lane-palette r7 — implement the fix r6 pinned

At bba4d3c. Read `lanes/palette-r6.report.md` first — it carries the root cause,
the range table, the fixture regen command and the oracle 3-print patch.

r6 did the decisive work: every symbol up through `read_palette_colors_y` is
bit-exact against the oracle (partition, skip, y_mode, uv_mode, palette_y_mode,
palette_y_size, colours — all rng-for-rng). Divergence begins exactly at
`decode_color_index_map`'s entry (oracle rng=51752, ours 39432). Cause is call
ORDER, not a table: libaom reaches `decode_color_map_tokens` from
`parse_decode_block` (`decodeframe.c:1135`, `av1_visit_palette`) AFTER the whole
mode-info decode including UV palette mode_info and filter_intra; we call
`decode_color_index_map` inline right after the Y colours (`decode.rs` ~3023)
and never read UV palette mode_info at all — which this gate's block genuinely
has (`uv_mode = UV_DC_PRED`, chroma-referenced).

## Do the fix, in this order, committing each green step
1. Port UV palette mode_info — `palette_uv_mode_cdf`, `palette_uv_size_cdf`,
   `read_palette_colors_uv` — into `read_intra_mode`'s palette branch,
   immediately after the Y colours. New adapting CDFs need all four wiring
   sites: struct field, defaults array, save/restore, and the per-frame counter
   reset. `reset2`/`reset3` are length-generic and save/restore is a whole-struct
   `Clone`, so the defaults array is the one to hand-check — but confirm the
   counter reset covers them. A table missing from the reset gives right values
   at the wrong adaptation rate, which reads as a slow drift, not a desync
   ([[cdf-counter-not-reset]]).
2. Move BOTH the Y and UV `decode_color_index_map` calls to run after
   `filter_intra` is read, matching libaom's `decode_mbmi_block` →
   `av1_visit_palette` order.
3. Re-run r6's exact trace recipe and confirm the ranges match to the END of the
   block, not just past the old divergence point.
4. Only with the gate pixel-exact: flip it green, remove the palette-Y refusal,
   and update `refusal_inventory.rs`'s pinned list — all in ONE commit. A branch
   that lifts a refusal without a gate does not merge to main.

Then merge `main` into this branch (main has moved a long way: multi-tile,
delta_q/delta_lf, a `bit_depth != 8` refusal, three reworded partition
refusals) and resolve it yourself, so the lane arrives mergeable.

Note `gate_coverage.rs` still lists `enable-palette` only if this lane's gate
does not enable it — when your gate is green with the tool on, that entry comes
off the list in the same commit.

Oracle hygiene: the aomenc checkout at `~/.cache/aom-oracle` is SHARED with five
sibling lanes. r6 patched `decodemv.c` for its trace and reverted it after use —
do the same, and never leave the shared oracle modified or half-rebuilt. Prefer
adding a permanent env-gated rung to `scripts/instrument-aom-oracle.sh` (silent
when unset, idempotent) over a throwaway patch.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge this branch into main. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. End with `lanes/palette-r7.report.md`, VERDICT on line 1.
