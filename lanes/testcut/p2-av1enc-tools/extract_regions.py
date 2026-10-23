#!/usr/bin/env python3
# Reads a llvm-cov export JSON (cargo llvm-cov report --json) and prints the executed
# source line ranges as TSV: label, file, line_start, line_end (merged per file).
import json, sys, collections

label, path = sys.argv[1], sys.argv[2]
data = json.load(open(path))["data"][0]
files = collections.defaultdict(list)
for fn in data["functions"]:
    names = fn["filenames"]
    for r in fn["regions"]:
        lc, _, le, _, count, fid, _, kind = r[:8]
        if count > 0 and kind == 0:
            files[names[fid]].append((lc, le))
out = []
for f, spans in files.items():
    if "/crates/ec-av1/src/" not in f:
        continue
    spans.sort()
    cs, ce = spans[0]
    for s, e in spans[1:]:
        if s <= ce + 1:
            ce = max(ce, e)
        else:
            out.append((f, cs, ce)); cs, ce = s, e
    out.append((f, cs, ce))
for f, s, e in out:
    print(f"{label}\t{f.split('/crates/ec-av1/')[1]}\t{s}\t{e}")
