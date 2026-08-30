# lane-realworld r7 — merge main, then start high bit depth

At 48562d6. Read `lanes/realworld-r6.report.md`.

## Job 1 — merge `main` into this branch and resolve it yourself
`git merge main` from this worktree. Main is at 92d8beb and has landed a lot
since this branch forked: multi-tile decode (gated), CDEF index reads, chroma
smooth/paeth/directional, a `bit_depth != 8` refusal, three reworded partition
refusals, dead-binding cleanup in `decode_stream`, and three guard tests. The
merge conflicts in nine places — eight in `stream.rs` (mostly gate bodies) and
one in `decode.rs`'s inter tile function, where main's per-tile loop meets your
`DeltaParams` threading. **You** resolve it: a wrong resolution there is a
silent decode bug, and you know which side owns each hunk. Suite green, then
COMMIT. Your delta_q/delta_lf work is finished and gated (40/40 pixel-exact,
`delta_q_hits`/`delta_lf_hits` = 1920 each), so this is the last thing standing
between it and main.

Keep, while resolving: main's `refusal_inventory.rs` entry
`"a stream whose bit depth is not 8 (this decoder reconstructs into 8-bit planes)"`,
and the reworded partition refusals — do not reintroduce any string claiming an
encoder never writes a case.

## Job 2, if turns remain — high bit depth
This is the critical path to the user's own files. Both AV1 files in this box's
library (`~/Downloads/The.Hunger.Games...2160p.AV1.HDR10...mkv` and
`~/Videos/Films/Troy...1080P.AV1...mkv`) are `yuv420p10le`, and both now stop on
that bit-depth refusal. `Picture`'s `y`/`u`/`v` are `Vec<u8>`; nothing in
`decode.rs` reads `bit_depth` at all, though `transform.rs` already takes and
uses it (`row_clamp = bit_depth + 8`, `col_clamp = max(bit_depth + 6, 16)`).

Scope for this round is the SHAPE, not the whole feature: work out what widening
the planes costs — `Picture` and `PlaneBuf` sample type, the prediction and
filter paths that assume `u8`, and what `ffmpeg_decode_sequence` must do to
produce a 10-bit reference for a gate. Write it down as a typing plan the way
`lanes/realworld-r3.report.md` did for delta_q; that report is why r5 could
implement both halves in one round. Land nothing half-wired.

The two films are the eventual gate: probe them with
`cargo run -p ec-av1 --example decode_probe -- <stream.obu>`, extracting with
`ffmpeg -i <file> -t 3 -c:v copy -f obu` (note `-f obu` — an IVF decodes as zero
frames rather than as an error). Expect a new refusal to appear behind the
bit-depth one; that is progress, and its name is the next lane's charter.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main` (merging main INTO this branch is job 1 and is fine). 75-turn
cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/realworld-r7.report.md`, VERDICT on line 1.
