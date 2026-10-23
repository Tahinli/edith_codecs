#!/usr/bin/env bash
# Print `-p <crate>` lines for the workspace crates affected by a change, for use as
#   cargo test $(scripts/changed-crates.sh)
#
# Changed files = `git diff <base>...HEAD` + uncommitted (staged/unstaged) + untracked.
# Each changed file maps to the workspace package whose directory contains it. Those
# packages are then expanded to all their reverse dependencies inside the workspace
# (path dependencies only). A change to a root file (Cargo.toml, Cargo.lock, .config/,
# .cargo/, rust-toolchain*) selects every workspace crate.
# ec-hw and ec-va are dropped unless they changed themselves (GPU; no hardware encode).
#
# Usage: scripts/changed-crates.sh [BASE_REF]   (BASE_REF defaults to main)
set -euo pipefail

base="${1:-main}"
root="$(git rev-parse --show-toplevel)"
cd "$root"

meta_file="$(mktemp)"
trap 'rm -f "$meta_file"' EXIT
cargo metadata --format-version 1 --no-deps > "$meta_file"

changed="$({
    git diff --name-only "$base...HEAD"
    git diff --name-only HEAD
    git ls-files --others --exclude-standard
} | sort -u | jq -R -s 'split("\n") | map(select(length > 0))')"

jq -r --argjson files "$changed" '
  .workspace_root as $wr
  | [ .packages[] | select(.source == null)
      | { name,
          dir: (.manifest_path | sub("/Cargo.toml$"; "") | ltrimstr($wr + "/")),
          deps: [ .dependencies[] | select(.path != null) | .name ] } ] as $pk
  | ($files | any(test("^(Cargo\\.toml|Cargo\\.lock|rust-toolchain[^/]*|\\.config/.*|\\.cargo/.*)$"))) as $rootChanged
  | [ $pk[] | select(.dir != "") as $p
      | select([$files[] | select(startswith($p.dir + "/"))] | length > 0)
      | .name ] | unique as $changedPkgs
  | (if $rootChanged then [$pk[].name] else $changedPkgs end) as $seeds
  | def closure($cur):
      ([ $pk[] | select(any(.deps[]; . as $x | any($cur[]; . == $x))) | .name ] + $cur | unique) as $next
      | if ($next | length) == ($cur | length) then $cur else closure($next) end;
    closure($seeds)
  | map(. as $n
      | select((["ec-hw", "ec-va"] | any(. == $n) | not) or ($changedPkgs | any(. == $n))))
  | sort[]
  | "-p " + .
' "$meta_file"
