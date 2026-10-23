# edith lane conventions (on top of ORCHESTRATION.md)

edith lives at `/home/user/Documents/Code/Rust/edith`. It depends on ec-* crates by path
(`../../../edith_codecs/crates/<name>`), so an edith worktree at `~/.cache/wt/edith-<name>`
resolves to `~/.cache/wt/edith_codecs`, a symlink to the edith_codecs main checkout, so local
builds work too. Heavy builds still go through remote-run, which lays out both repos side by side.

## Setup for edith lane `<name>`
- `git -C /home/user/Documents/Code/Rust/edith worktree add ~/.cache/wt/edith-<name> -b lane-<name> main`
- Never touch the main edith checkout. Its working tree may hold someone else's uncommitted changes.
- Build and test from the edith_codecs main checkout (or your edith_codecs lane worktree,
  if the lane also changes an ec-* crate):
  `cd /home/user/Documents/Code/Rust/edith_codecs && EDITH_TREE=~/.cache/wt/edith-<name> scripts/remote-run.sh edith-<name> --with-edith -- cargo test -p engine --release`
  The command runs inside the remote edith tree.
- edith tests that need `real_library.toml` media skip on the server (the media is only on the
  local machine). If your lane's acceptance needs a real-library clip, run that one test
  locally as well, from the worktree, with a private target dir. Ask yourself whether it
  is worth it first: local runs are the fallback.

## Rules
- edith commit style: gitmoji + `type(scope): subject`, e.g. `✨ feat(audio): …`, `🐛 fix(demux): …`,
  `♻️ refactor(export): …`. No Co-Authored-By trailer or other AI attribution line.
- Every existing edith test must still pass. Never delete or `#[ignore]` a test to get there.
- `cargo clippy -p <crate> --all-targets -- -D warnings` must not gain new warnings on the edith
  crates you touch. Compare against main if edith is not clippy-clean today.
- Write the report to `~/.cache/wt/edith-<name>.report.md` (outside every tree). The orchestrator
  copies it into edith_codecs/lanes/. Commit only code and tests on your edith branch.

## Where edith lanes land
The orchestrator merges finished edith lanes into `codecs-integration` (worktree
`~/.cache/wt/edith-integ`), not into `main`. The main edith checkout has someone else's
uncommitted work, and another session may use it. `main` is fast-forwarded from
`codecs-integration` only when that checkout is safe to touch. New edith lanes branch from
`codecs-integration`.
