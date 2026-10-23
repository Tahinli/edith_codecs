# Lane preamble (read before your charter)

- Read `lanes/ORCHESTRATION.md` and follow it exactly. `DONE.md` is the spec.
- Never ask the user questions. Do not push. Commits carry no Co-Authored-By or any other
  AI attribution line.
- Your worktree is `~/.cache/wt/<name>` on branch `lane/<name>`, created from main. Run
  `scripts/link-fixtures.sh` inside it first.
- **Compute:** every cargo build, test, bench or fuzz run goes through
  `scripts/remote-run.sh <name> [--host auto|a|b|c] -- <cmd>`.
  - Use `--host auto` unless you need a host-specific resource. Host a has the HEVC
    reference YUVs in `~/refyuv`. Hosts b and c have the aom oracle in
    `~/.cache/aom-oracle`.
  - The user's own computer is a fallback only. The exceptions are libva and GPU work,
    which must run locally.
  - Inside `bash -c '...'`, single-quote the command so `$HOME` expands on the remote host.
  - The remote checkout is `$HOME/lanes/<name>/edith_codecs`. Fall back to
    `$HOME/lanes/<name>` if that path does not exist.
- **Waiting:** do not rely on background watchers or Monitor notifications. They have
  repeatedly failed to fire and left lanes idle for hours. Run jobs in the foreground with
  a long timeout (up to 600000 ms per tool call), or start them detached with output to a
  remote log and poll that log with `ssh remote-user@<host> tail -5 <log>` until it finishes.
- **Fuzzing:** `cargo +nightly fuzz run <target> -- -max_total_time=3600` per target. Run
  at most 4 targets in parallel on one host. Every crash gets a fix plus a replay
  regression test in the crate's `tests/`.
- **Finish:**
  - Write `lanes/<name>.report.md`: goal, base sha, changes, the exact verify commands and
    their results, and what is left.
  - Commit as `lane/<name>: ...`.
  - In your final message give the branch, the sha, the verify results and any blockers.
    Never claim a result you did not observe.
