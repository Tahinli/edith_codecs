# lane remoterun: scripts/remote-run.sh

## Goal
Run heavy cargo commands on the two remote-user servers (host a, host b) instead of this
machine, per lane `remoterun`. Shell scripting only; no Rust code changed.

Base sha: 6120482d (main).

## What changed
- `scripts/remote-run.sh` (new). Usage:
  `scripts/remote-run.sh <lane> [--host auto|a|b] [--with-edith] [--clean] -- <command...>`
  - Source tree = `git rev-parse --show-toplevel` of the cwd (works in worktrees).
  - rsync to `~/lanes/<lane>/edith_codecs/` with `-a --delete`, excluding `target/`,
    `/fixtures/`, `/.git`. Remote `git init -q` if `.git` is missing (the `.cargo`
    runner needs `git rev-parse --show-toplevel`). Remote symlink
    `fixtures -> ~/fixtures` if absent (dangling is fine).
  - `--with-edith` also syncs `$EDITH_TREE` (default `/home/user/Documents/Code/Rust/edith`)
    to `~/lanes/<lane>/edith/` (excluding `target/`, `/assets/`, `/.git`) and runs there.
    Its path deps `../../../edith_codecs` resolve to `~/lanes/<lane>/edith_codecs`.
  - Host choice: `auto` tries a then b, each under the remote `flock -n 9` on `~/.remote-slot`
    (exit 75 when busy). Retries every 20 s for 10 min, then runs locally with
    `CARGO_TARGET_DIR=~/.cache/tgt-<lane> TMPDIR=~/.cache/tmp-<lane> CARGO_BUILD_JOBS=4`.
    Explicit `--host` retries the same way and then exits 75 (busy) or 255 (unreachable).
  - Remote env: `CARGO_TARGET_DIR=~/.cache/tgt-<lane>`, `TMPDIR=~/tmp/<lane>`,
    `CARGO_BUILD_JOBS=4`, `EC_MEMGUARD_MAX=6G` (a local `EC_*` value wins). All local `EC_*`
    variables are forwarded.
  - The remote script is passed on stdin to `bash -c "$(cat)"`, so no quoting reaches fish.
  - stderr prints `remote-run: host=a|b|local` when a run starts. Exit code is the remote's.
  - `--clean` removes `~/lanes/<lane>`, `~/.cache/tgt-<lane>`, `~/tmp/<lane>` on both hosts,
    and `~/.cache/tgt-<lane>`, `~/.cache/tmp-<lane>` locally.
- `scripts/memguard-runner.sh`: all `MemoryMax=10G` became `MemoryMax=${EC_MEMGUARD_MAX:-10G}`
  (5 occurrences), header comment updated.

## Verify commands and results
Run from `~/.cache/wt/remoterun` on branch `lane/remoterun`.

1. `scripts/remote-run.sh remoterun-selftest --host a -- bash -c 'hostname; nproc; ls fixtures/ ; git rev-parse --show-toplevel'`
   PASS. `host=a`, `fedora-8gb-nbg1-2`, nproc 4, fixtures listing `vectors`,
   toplevel `/home/user/lanes/remoterun-selftest/edith_codecs`.
2. Same with `--host b`. PASS. `host=b`, `fedora-8gb-nbg1-3`, nproc 4, `vectors`, same toplevel.
3. `scripts/remote-run.sh remoterun-selftest -- cargo build -p ec-core` (auto). PASS. `host=a`,
   `Compiling ec-core v0.1.0`, `Finished dev profile`, exit 0.
4. `scripts/remote-run.sh remoterun-selftest --host a -- bash -c 'exit 3'`. PASS. Returned 3.
5. `scripts/remote-run.sh remoterun-selftest --clean`. PASS. Remote lane dir, tgt and tmp removed on
   both hosts, local tgt/tmp removed. `~/fixtures` still present on host a.

Not exercised: the busy-slot retry loop, the 10-minute timeout, the local fallback, and
`--with-edith`. The edith layout was checked by reading the path-dep rule, not by building.

## Known limits
- Exit 75 is used for "slot busy", so a command that itself exits 75 is treated as busy and
  retried on the next host. Documented in the script header.
- `--with-edith` with the local fallback runs in `$EDITH_TREE`, which may not be this worktree.
- Remote `fixtures` symlink is made only in `edith_codecs`, not in `edith`.
