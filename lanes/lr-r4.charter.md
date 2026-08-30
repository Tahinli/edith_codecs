# lane-lr r4 — the Wiener and self-guided filters

At ebac7a0. Read `lanes/lr.report.md`'s r3 section only.

## State — the symbols are read AND proven
r3's gate `a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly`
runs a real `--enable-restoration=1` stream: 39/40 attempts reach the LR
refusal with zero other refusals, `wiener_hits=19`, `sgrproj_hits=72`.
`RestorationGrid` and the per-plane `(WienerInfo, SgrprojInfo)` are populated
correctly. Suite 236/0.

r3 also found and fixed a real bug r2's report had claimed away: the `uses_lr`
refusal ran BEFORE the tile decode, so `read_lr` was never invoked through
`decode_stream` at all — 40/40 attempts had zero `read_lr_unit` hits until the
check moved after the tile decode.

Two gate facts r3 paid for: `--enable-restoration=1` only actually lands
`uses_lr = true` on this fixture at `cq-level <= 20` (the sibling gates' usual
45 never fires it), and `--sb-size=64` is required or aomenc's default 128px
superblock lands in a dead-ended partition gap and eats the attempt window.
And a dead end: do NOT hard-require `switchable_hits() > 0` — aomenc's RD may
pick a frame-level fixed Wiener or Sgrproj type instead. Assert the SUM.

## Your job
1. **Wiener.** `apply_loop_restoration` after `apply_cdef` in `decode.rs` (two
   call sites, ~4983 key-frame and ~10502 inter-frame), driven by
   `RestorationGrid`. The 3-pixel stripe boundary save/restore (libaom's
   `rlbs`) is the trap: 64-row stripes, and the filter reads rows the next
   stripe has already overwritten unless you save them first. COMMIT.
2. **Self-guided**, incl. the box-sum radii and `SGR_PARAMS`. COMMIT.
3. Widen r3's gate to assert `Ok` and pixel-exactness instead of the named
   refusal, then drop the `stream.rs` refusal. COMMIT.

Ordering fact from a sibling lane: libaom runs `superres_post_decode()` between
`av1_cdef_frame` and `av1_loop_restoration_filter_frame` — superres before LR.

Merge note: main is at 06d29ee with `gate_coverage.rs` (pins the aomenc tools no
gate exercises — `enable-restoration` is one gate from that list) and
`refusal_inventory.rs` (pins every decode-path refusal string). Report every
refusal string you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge, never touch `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN
STEP. Update `lanes/lr.report.md` with an r4 section, VERDICT first.
