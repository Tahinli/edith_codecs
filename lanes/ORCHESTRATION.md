# Lane conventions (orchestrated run from 2026-10-15)

`DONE.md` is the spec. Every lane serves one item in it, and its charter names the
edith-visible effect it buys.

## Setup for lane `<name>`
- Repo: `/home/user/Documents/Code/Rust/edith_codecs` (edith lanes: `/home/user/Documents/Code/Rust/edith`).
- Worktree: `git -C <repo> worktree add ~/.cache/wt/<name> -b lane/<name> main`
  (edith: branch `lane-<name>`, worktree `~/.cache/wt/edith-<name>`).
- Fixtures: `scripts/link-fixtures.sh` from inside the worktree (edith_codecs only).
- Never commit to `main`. The orchestrator merges.

## Where computation runs
Heavy work (cargo build/test/clippy/bench/fuzz) runs on the servers through
`scripts/remote-run.sh <name> -- <command>` once that script exists. Before then, or if it
is unavailable, run locally with:
`CARGO_TARGET_DIR=~/.cache/tgt-<name> TMPDIR=~/.cache/tmp-<name>` (create the tmp dir;
never write into /tmp). Limit local builds to `-j4`.
- Use `--host auto` (the default). `--host a|b` never falls back and exits 75 when the slot
  stays busy. Under auto, the local fallback is a real run: cargo-fuzz and nightly are
  installed locally too.

## Rules
- Errors: `ec_core::Error::unsupported(what, why)` for out-of-scope input and
  `Error::corrupt(..)` for malformed input. No `unwrap`/`expect`/`panic!` on input-dependent
  paths. No `debug_assert` as the only guard before indexing.
- No source file over 5,000 lines. Put new code in new modules.
- `cargo clippy -p <crate> --all-targets -- -D warnings` must be clean on the crates you touch.
  An `#[allow]` needs a one-line reason comment.
- Match the surrounding code's style and comment density. Pure Rust only (no C, no asm).
- Never delete or `#[ignore]` an existing test to make things pass.

## Finish
1. Write `lanes/<name>.report.md`: goal, base sha, what changed, the exact verify commands
   and their results (pass counts, exact/inexact tables), and what is left.
2. Commit on `lane/<name>` as `lane/<name>: <summary>`. No Co-Authored-By trailer or any other
   AI attribution line in commit messages (the user's rule, 2026-10-16).
3. In your final message, give the branch, the commit sha, the verify commands with
   pass/fail, and any blocker. If you could not finish, say exactly what fails. Do not
   claim success you did not observe.
