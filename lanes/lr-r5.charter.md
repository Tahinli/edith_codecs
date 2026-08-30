# lane-lr r5 — finish Wiener, then self-guided

At 06ba9af — r4's Wiener work, committed verbatim by the orchestrator at its cap
and **never seen to compile**. `lanes/lr-r4.charter.md` is still binding; read it
and the r3 section of `lanes/lr.report.md`.

1. `cargo check`, fix whatever r4 left mid-edit, then the full suite
   (baseline 236/0 on this tree). Getting 06ba9af green is the whole job until
   it is green. COMMIT.
2. Finish Wiener: `apply_loop_restoration` after `apply_cdef` in `decode.rs`
   (two call sites, key-frame and inter-frame), driven by `RestorationGrid`.
   The 3-pixel stripe boundary save/restore (libaom's `rlbs`) is the trap:
   64-row stripes, and the filter reads rows the next stripe has already
   overwritten unless you save them first. COMMIT.
3. Self-guided, incl. the box-sum radii and `SGR_PARAMS`. COMMIT.
4. Widen the r3 gate to assert `Ok` and pixel-exactness instead of the named
   refusal, then drop the `stream.rs` refusal. COMMIT.

Facts already paid for: the symbols are read and gate-proven
(`wiener_hits=19`, `sgrproj_hits=72` on a real stream); `--enable-restoration=1`
only lands `uses_lr` at `cq-level <= 20`; `--sb-size=64` is required or aomenc's
default 128px superblock lands in a dead-ended partition gap; do NOT
hard-require `switchable_hits() > 0` — assert the SUM of the three arms, since
aomenc's RD may pick a frame-level fixed type. Ordering: libaom runs superres
between CDEF and loop restoration.

Merge note: main is at 53b319b with `gate_coverage.rs` (pins the aomenc tools no
gate exercises) and `refusal_inventory.rs` (pins every decode-path refusal
string). Report every refusal string you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. Update `lanes/lr.report.md` with an r5 section, VERDICT first.
