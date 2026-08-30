# lane-palette r2 — type in the plan

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-palette`, branch
`lane-palette`, at 6ffa519.

## Your round is TYPING, not derivation
r1 spent its entire budget deriving a complete, line-referenced stage-1 plan
against the real libaom source and wrote it to `lanes/palette.report.md`. It
covers: the CDF table contents and shapes, the four-site `cdf_state.rs` wiring,
the `Neighbours` additions plus a new `record_palette_y`, the
`palette_mode_ctx` / `palette_uv_mode_ctx` fix (the charter's named trap), the
colour-cache merge and delta-colour reads, the wavefront colour-index-map decode
with its context function, and the decision to wire palette prediction into
`PlaneBuf::reconstruct` through a new `PALETTE_PRED` thread-local (the existing
`ENABLE_EDGE_FILTER` idiom) rather than threading a parameter through 32 call
sites.

**Do not re-derive any of it. Do not re-read libaom for anything the report
already states.** Three lanes in a row have now burned a full round on recon
(class `worker-cap-spent-reading`). Start at the report's section 1 and type
your way down, compiling as you go.

`lanes/palette.charter.md` (r1's) still governs the staging, the gate rules and
the hard rules — read it once, briefly.

## Order — COMMIT AFTER EVERY GREEN STEP, and commit early
1. Sections 1-2 (CDF tables + `cdf_state.rs` wiring). `cargo check`. COMMIT —
   inert additions, but they are the four-site wiring and they are worth having
   on the branch.
2. Sections 3-5 (Neighbours, contexts, colour cache, delta colours, index map).
   COMMIT when it compiles.
3. Section 6 (reconstruction through `PALETTE_PRED`) + remove the two Y
   refusals. Full suite. COMMIT.
4. The gate. Palette is a screen-content tool: aomenc only picks it on flat,
   few-colour, repetitive content, so the fixture likely needs a synthetic
   few-colour pattern rather than a gradient — and whatever you use must be
   deterministic, so hash it twice and prove it. `--enable-palette=1`, probably
   `--tune-content=screen`. HARD-assert a thread-local `Cell<usize>` count of
   palette blocks reconstructed. COMMIT.

r1 flagged one open judgment call: split-luma-TU palette blocks — support them
via per-TU prediction slicing, or refuse by name if that proves error-prone.
Decide it by testing against a real `--enable-palette=1` stream, not by guessing.
A named refusal there is an acceptable outcome for this round.

A sizing lesson from a sibling lane, which applies to your gate: a per-superblock
symbol cannot fire in a single-superblock fixture. Size the frame so the encoder
has something to choose between.

## Note for the merge
Main carries two guards you do not have: `gate_coverage.rs`, which pins the
aomenc tools no gate exercises (`enable-palette` and `enable-intrabc` are both
on that list, and your gate deletes the palette entry), and
`refusal_inventory.rs`, which pins every decode-path refusal string so adding or
removing one fails until the list is updated. Report the refusal strings you add
or remove, verbatim.

## Hard rules
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`; foreground builds
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees (edith_codecs, -chroma, -realworld, -lr, -superres, -tiles) have live
agents — never build in or edit them. Never push, never merge, never touch
`main`. 75-turn cap, does not reset. End with `lanes/palette-r2.report.md`,
VERDICT on line 1.
