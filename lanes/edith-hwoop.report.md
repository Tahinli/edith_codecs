# lane-hwoop report

Branch `lane-hwoop`, head `1616c67b`, worktree `~/.cache/wt/edith-hwoop`.

## What moved

Hardware decode and encode left the dlopen'd C-ABI plugin and now run in an
out-of-process child, `hw-child`, built from `crates/engine-hw/src/bin/hw-child.rs`
(logic in `crates/engine-hw/src/child.rs`). The old `crates/engine/src/bin/hw-encode-child.rs`
is removed. `crates/engine/src/hw.rs` and `crates/engine/src/hwproc.rs` now talk to
the child over IPC. `crates/engine-hw` is `#![forbid(unsafe_code)]` in `lib.rs`, and the
child binary carries the same attribute.

## IPC protocol summary

- Transport: Unix domain socket between the editor and the child.
- Pictures: memfd (or dmabuf) descriptors passed with SCM_RIGHTS, so frame data is not copied over the socket.
- Lifetime: the child sets PDEATHSIG so it dies with the editor.
- Failure: if the child dies, the editor falls back to software for the rest of the job.

## Gates (server)

- Full gate: lane 3749 passed / 44 failed. Base (main) 3748 passed / 43 failed.
- Main's failures come from the debug-mode Opus bug (fixed on codecs main) and timing flakes.
- The two lane-only failures, rerun alone, pass: `compose_at` 14/14 and `edith_mcp auto_av1_start_line` 1/1. No lane regressions.
- `cargo clippy --workspace --all-targets -- -D warnings` on server: exit 0 (CLIPPY=0), after the forbid attribute was added to `hw-child.rs`.

## Local GPU decode tests

Not run. The local machine stayed over the load limit (load average under 8 required)
for the whole one-hour window. The last check read load 68.8 with 20 GB free.
The test command is ready to run once the machine is quiet:

    CARGO_TARGET_DIR=~/.cache/tgt-hwoop TMPDIR=~/.cache/tmp-hwoop nice -n 19 cargo test -j4 -p engine --test hw_decode --test hw_midgop_seek --test hw_fallback -- --include-ignored

No hardware encode was run, and none is part of this lane's verification.

## Remaining unsafe

- `grep -rn unsafe crates/engine/src/hw.rs crates/engine/src/hwproc.rs crates/engine-hw/src` shows only comments.
- `libloading` remains in `crates/engine/Cargo.toml` because `crates/engine/src/ao.rs` still uses it. That is outside this lane's scope.

## Local GPU decode tests (coordinator, 2026-10-22 13:45)

Decode only, no hardware encode run. `nice -n 19 cargo test -j4 --no-fail-fast -p engine --test hw_decode --test hw_midgop_seek --test hw_fallback -- --include-ignored`:
hw_decode 13 passed, hw_fallback 1 passed, hw_midgop_seek 2 passed, 0 failed.

Two prerequisites that the first attempts tripped on:
- the worktree needs the edith checkout's untracked `assets/` media (symlinked); without it the tests fail with `NotFound`;
- `hw-child` must be built first (`cargo build -p engine-hw --bin hw-child`). `cargo test -p engine` does not build it, and without it every hw test fails with "no hardware decode plugin/driver available".
