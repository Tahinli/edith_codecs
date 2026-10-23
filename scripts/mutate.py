#!/usr/bin/env python3
"""Apply or revert one cargo-mutants mutant in place, from `cargo mutants --list --json`.

Used by scripts/kill-matrix.sh on a scratch copy of the tree (never on the local checkout).

  mutate.py apply  <mutants.json> <index> <root>   # writes the mutated file, saves the original
  mutate.py revert <root>                         # restores every file saved by apply

The original bytes are kept in <root>/.mutate-backup/ so revert is exact.
"""
import json
import os
import shutil
import sys


def offset(text_lines, line, col):
    # cargo-mutants spans are 1-based line and 1-based char column (inclusive start, exclusive end).
    return sum(len(l) for l in text_lines[: line - 1]) + (col - 1)


def apply(json_path, index, root):
    mutants = json.load(open(json_path))
    m = mutants[index]
    rel = m["file"]
    path = os.path.join(root, rel)
    backup_dir = os.path.join(root, ".mutate-backup")
    os.makedirs(backup_dir, exist_ok=True)
    backup = os.path.join(backup_dir, rel.replace("/", "__"))
    if not os.path.exists(backup):
        shutil.copy2(path, backup)
    with open(path, encoding="utf-8", newline="") as f:
        text = f.read()
    lines = text.splitlines(keepends=True)
    s = m["span"]["start"]
    e = m["span"]["end"]
    a = offset(lines, s["line"], s["column"])
    b = offset(lines, e["line"], e["column"])
    original = text[a:b]
    new = text[:a] + m["replacement"] + text[b:]
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(new)
    print(f"mutant {index}: {rel}:{s['line']}:{s['column']} {original!r} -> {m['replacement']!r}")


def revert(root):
    backup_dir = os.path.join(root, ".mutate-backup")
    if not os.path.isdir(backup_dir):
        return
    for name in os.listdir(backup_dir):
        rel = name.replace("__", "/")
        shutil.copy2(os.path.join(backup_dir, name), os.path.join(root, rel))
        os.remove(os.path.join(backup_dir, name))


if __name__ == "__main__":
    if sys.argv[1] == "apply":
        apply(sys.argv[2], int(sys.argv[3]), sys.argv[4])
    elif sys.argv[1] == "revert":
        revert(sys.argv[2])
    else:
        sys.exit(__doc__)
