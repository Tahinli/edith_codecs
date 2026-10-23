# Usage: pick.py <list.json> <out-dir> <cap-per-candidate> <seed> <T1> [T2 ...]
# Reads cargo-mutants --list --json and each candidate's regions.json (from regions.py).
# Writes <out-dir>/sample.json: {"names": [...], "by_test": {T: [names]}, "tags": {name: [T,...]}}.
import json, random, sys, os
lst, out, cap, seed = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
tests = sys.argv[5:]
muts = json.load(open(lst))
def name_of(m):
    return m.get('name') or '%s:%s:%s' % (m['file'], m['line'], m.get('replacement', ''))
rng = random.Random(seed)
by_test, tags = {}, {}
for t in tests:
    reg = json.load(open(os.path.join(out, t + '.regions.json')))
    cov = [(r[0], r[1], r[2]) for r in reg['covered']]
    U = [(r[0], r[1], r[2]) for r in reg['U']]
    def hits(rs, m):
        for fn, ls, le in rs:
            if fn.endswith(m['file']) and ls <= m['line'] <= le:
                return True
        return False
    inU = [m for m in muts if hits(U, m)]
    inC = [m for m in muts if hits(cov, m) and not hits(U, m)]
    rng.shuffle(inU); rng.shuffle(inC)
    pick = (inU + inC)[:cap]
    names = [name_of(m) for m in pick]
    by_test[t] = {'U_mutants': len(inU), 'covered_mutants': len(inC), 'sampled': len(names), 'names': names}
    for n in names:
        tags.setdefault(n, []).append(t)
allnames = sorted(tags)
json.dump({'names': allnames, 'by_test': by_test, 'tags': tags}, open(os.path.join(out, 'sample.json'), 'w'), indent=1)
print(json.dumps({t: {k: v for k, v in d.items() if k != 'names'} for t, d in by_test.items()}), 'union=%d' % len(allnames))
