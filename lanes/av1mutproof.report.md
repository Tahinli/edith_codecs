# lane-av1mutproof — targeted mutation proof for two declared-debt gates

Base: `a21f3680`. Branch: `lane-av1mutproof`. Tree: `~/.cache/wt/mutproof`.

Debt (lanes/av1formatsweep.report.md): two 4:4:4 >8-bit gates carried only the
oracle byte-compare plus header/counter non-vacuity, so neither was shown to bite
the decoder site its cell depends on. This lane proves each one, at the site the
cell's own doc comment names — not a nearby helper.

## Result: both gates bite. No blind gate, no source change.

| gate | mutated site (file:line) | observed failure output | reverted-green output |
|---|---|---|---|
| `a_lossless_444_10bit_inter_stream_decodes_pixel_exact` | `crates/ec-av1/src/decode.rs:11765` — `let (cu_x, cu_y) = (cpx + cu_col * 4, cpy + cu_row * 4);` mutated to `cu_col * 8`, the x stride of the per-4x4 chroma unit walk that is the gate's own `rect_split_lossless_chroma444_hits()` site (doc: "the per-4x4 chroma unit walk a lossless HORZ/VERT strip takes at subsampling 0/0"). Real arithmetic (unit x offset), not an added assertion. | `panicked at crates/ec-av1/src/stream.rs:8758:17:`<br>`a_lossless_444_10bit_inter_stream_decodes_pixel_exact: decode-order frame 0 of 6 (6 shown, 0 hidden) differs from the oracle at byte 37000 (ours 0 vs 192), 8613 bytes differ`<br>`test result: FAILED. 0 passed; 1 failed` | `a_lossless_444_10bit_inter_stream_decodes_pixel_exact: 6 frames byte-exact vs aomdec at 10-bit 4:4:4`<br>`test result: ok. 1 passed; 0 failed` |
| `a_444_12bit_inter_sequence_decodes_pixel_exact` | `crates/ec-av1/src/mc.rs:55` — `round_delta`: `bd.saturating_sub(10)` → `bd.saturating_sub(9)`. This is the 12-bit-only arithmetic behind the gate's `mc::mc_subpel_hits()` claim ("the 12-bit round pair", 3/11 at 8 and 10 bits, 5/9 at 12): at 12 bits it shifts the pair to 6/8 and at ≤10 bits it is a no-op, so the mutation is specific to this cell. | `panicked at crates/ec-av1/src/stream.rs:8758:17:`<br>`a_444_12bit_inter_sequence_decodes_pixel_exact: decode-order frame 0 of 6 (6 shown, 0 hidden) differs from the oracle at byte 24584 (ours 109 vs 110), 2426 bytes differ`<br>`test result: FAILED. 0 passed; 1 failed` | `a_444_12bit_inter_sequence_decodes_pixel_exact: 6 frames byte-exact vs aomdec at 12-bit 4:4:4`<br>`test result: ok. 1 passed; 0 failed` |

Both failures are pixel-level, on frame 0, inside the byte-compare — i.e. each
gate's oracle compare itself is shown live, not just its header/counter arms.

## Method notes (two traps hit, both caught)

1. **The mutation must be in the tree cargo actually builds.** The first
   `decode.rs` edit landed in the primary checkout (`edith_codecs`) because the
   edit tool's cwd is the session repo, not the worktree — the test then ran
   green on an unmutated worktree and looked like a blind gate. Reverted the
   primary checkout immediately (`git status` clean) and re-applied with an
   absolute worktree path. Lesson: in a worktree lane, verify
   `grep <mutation> <worktree-path>` before reading a green run as a finding.
2. **A stale binary silently returns "no failure".** `round_delta`'s first
   mutated run printed no `Compiling` line (mtime unchanged) and reported green.
   `touch crates/ec-av1/src/mc.rs` forced the rebuild, which then failed as
   expected. Lesson: treat a mutation run without a `Compiling` line as
   inconclusive, not as a surviving mutation.
3. Local runs use `EC_NOMEMGUARD=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target`
   and `scripts/memguard-runner.sh` copied into the worktree (per
   skill://edith-worktree-memguard-runner); `EC_AV1_REQUIRE_AOMENC=1` so a
   missing oracle cannot SKIP the gates into a false green.

## Committed artifacts

`lanes/av1mutproof.report.md` only. No source change survives; both gates are
sensitive to their cell's site, so no "sensitive to" comment was needed in
either gate body.
