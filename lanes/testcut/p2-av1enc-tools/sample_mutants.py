#!/usr/bin/env python3
# usage: sample_mutants.py <mutants.json> <N> <seed> <cov.tsv>... > sample.tsv
# Keeps mutants whose span lines are executed by at least one candidate heavy test (region-level
# coverage from cov-shard.sh), then draws N of them stratified by file (round-robin over files in
# a seeded shuffled order). Output: name, file, line span, covered_by (candidates).
import collections, json, random, sys

mut_path, n, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
cov = collections.defaultdict(lambda: collections.defaultdict(list))  # file -> label -> spans
fast = set()
for p in sys.argv[4:]:
    for line in open(p):
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 4:
            continue
        label, f, lo, hi = parts
        if label == "FAST":
            fast.add((f, int(lo), int(hi)))
            continue
        cov[f][label].append((int(lo), int(hi)))

def hit(f, lo, hi, spans):
    return any(not (b < lo or a > hi) for a, b in spans)

muts = json.load(open(mut_path))
pool = collections.defaultdict(list)
for m in muts:
    f = m["file"].split("crates/ec-av1/", 1)[-1]
    if f not in cov:
        continue
    lo, hi = m["span"]["start"]["line"], m["span"]["end"]["line"]
    by = [lab for lab, sp in cov[f].items() if hit(f, lo, hi, sp)]
    if by:
        pool[f].append((m["name"], f, lo, hi, sorted(by)))
total = sum(len(v) for v in pool.values())
rng = random.Random(seed)
for v in pool.values():
    v.sort()
    rng.shuffle(v)
files = sorted(pool)
rng.shuffle(files)
picked = []
i = 0
while len(picked) < n and any(pool.values()):
    f = files[i % len(files)]
    if pool[f]:
        picked.append(pool[f].pop())
    i += 1
    if i > 10 * len(files) * (n + 1):
        break
print(f"# pool={total} files_in_pool={sum(1 for v in pool.values() if v)} sampled={len(picked)} seed={seed}", file=sys.stderr)
print("name\tfile\tline_lo\tline_hi\tcovered_by")
for name, f, lo, hi, by in picked:
    print(f"{name}\t{f}\t{lo}\t{hi}\t{','.join(by)}")
