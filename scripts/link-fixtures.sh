#!/usr/bin/env bash
# link-fixtures.sh — give this worktree the primary checkout's root `fixtures/`.
#
# The root `fixtures/` directory is listed in `.gitignore` (line 2) and is NOT
# committed, so `git worktree add` gives every lane worktree a tree WITHOUT it.
# Every test that reads a fixture through `<crate>/../../fixtures` then reports
# green having checked nothing: this is the "gate skips on its own failure"
# class, and it has already cost false verdicts (lane-av1skipfix 2026-09-29).
#
# This script makes a SYMLINK, never a copy: the fixtures are large and the
# lane worktrees exist to run tests, not to duplicate data.
#
# Usage: scripts/link-fixtures.sh [--check]
#   (no args)  create the symlink, or no-op if fixtures/ is already there
#   --check    report what would happen and exit non-zero if the fixtures are
#              absent; for a batch preflight
#
# Idempotent and safe to run from any worktree of this repo.

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT" || exit 2

# `--git-common-dir` resolves to the PRIMARY checkout's .git from any linked
# worktree (a linked worktree's own .git is a FILE, not the common dir), so
# this works identically in the primary tree and in ~/.cache/wt/<lane>.
COMMON=$(git rev-parse --git-common-dir 2>/dev/null) || {
  echo "link-fixtures: not inside a git repository ($ROOT)" >&2
  exit 2
}
COMMON=$(cd -- "$COMMON" && pwd) || exit 2
PRIMARY=$(dirname -- "$COMMON")
SOURCE="$PRIMARY/fixtures"

CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

report() {  # report <state> <detail>
  echo "link-fixtures: $1 — $2"
  echo "  primary checkout: $PRIMARY"
  echo "  this tree:        $ROOT"
}

if [ -e "$ROOT/fixtures" ] || [ -L "$ROOT/fixtures" ]; then
  if [ -L "$ROOT/fixtures" ] && [ -d "$ROOT/fixtures" ]; then
    report "already linked" "$ROOT/fixtures -> $(readlink -f "$ROOT/fixtures")"
    exit 0
  fi
  if [ -d "$ROOT/fixtures" ]; then
    report "already a real directory" "nothing to do ($ROOT/fixtures)"
    exit 0
  fi
  report "REFUSING" "$ROOT/fixtures exists but is neither a directory nor a live symlink; remove it by hand" >&2
  exit 2
fi

if [ ! -d "$SOURCE" ]; then
  report "REFUSING" "the primary checkout has no fixtures/ at $SOURCE" >&2
  echo "  It is gitignored, so it exists only where someone generated it:" >&2
  echo "  scripts/gen-fixtures.sh (containers) and scripts/fetch-vectors.sh" >&2
  echo "  (conformance vectors). Run one there, then re-run this script." >&2
  exit 2
fi

if [ "$CHECK" = 1 ]; then
  report "would link" "$ROOT/fixtures -> $SOURCE"
  exit 1
fi

ln -s "$SOURCE" "$ROOT/fixtures" || exit 2
report "linked" "$ROOT/fixtures -> $SOURCE"
echo "  Fixtures are now reachable; run your crate's gates with"
echo "  EC_REQUIRE_FIXTURES=1 so an absent fixture FAILS instead of skipping."
