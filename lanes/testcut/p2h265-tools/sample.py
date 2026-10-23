# usage: sample.py OUT TEST MAXN  -> writes OUT/sample-TEST.json (names in priority order)
import json, sys, collections
out, test, maxn = sys.argv[1], sys.argv[2], int(sys.argv[3])
def tail(p):
    i = p.find("ec-h265/")
    return p[i + len("ec-h265/"):] if i >= 0 else None
def load(n):
    return json.load(open(f"{out}/{n}.json"))["data"][0]
def funcs(data):
    res = []  # (file, name, lo, hi, fast_hits_or_covered_hits, uncov_hits)
    for fn in data["functions"]:
        fl = [tail(fn["filenames"][r[5]]) for r in fn["regions"][:1]]
        if fl[0] is None:
            continue
        lo = min(r[0] for r in fn["regions"]); hi = max(r[2] for r in fn["regions"])
        res.append({"file": fl[0], "name": fn["name"], "lo": lo, "hi": hi,
                    "hits": sum(r[4] for r in fn["regions"]),
                    "covered_regions": {(r[0], r[1], r[2], r[3], r[7]) for r in fn["regions"] if r[4] > 0}})
    return res
def merged(lst):
    d = {}
    for f in lst:
        k = (f["file"], f["name"])
        if k not in d:
            d[k] = dict(f)
            continue
        m = d[k]
        m["hits"] += f["hits"]
        m["lo"] = min(m["lo"], f["lo"]); m["hi"] = max(m["hi"], f["hi"])
        m["covered_regions"] = m["covered_regions"] | f["covered_regions"]
    return d
fast = merged(funcs(load("fast")))
T = merged(funcs(load(test)))
cov = json.load(open(f"{out}/coverage-summary.json"))["per_test"][test]
ufuncs = {(x[0], x[1]) for x in cov["U_functions"]}
muts = json.load(open(f"{out}/mutants.json"))
cands = []
for m in muts:
    f = tail(m["file"]) if "ec-h265/" in m["file"] else m["file"]
    if f is None:
        continue
    line = m["span"]["start"]["line"]
    best = None
    for key, fn in fast.items():
        if key[0] != f or not (fn["lo"] <= line <= fn["hi"]):
            continue
        t = T.get(key)
        if t is None or not t["covered_regions"]:
            continue
        if best is None or (fn["hi"] - fn["lo"]) < (best["hi"] - best["lo"]):
            best = fn
    if best is None:
        continue
    key = (best["file"], best["name"])
    t = T.get(key)
    prio = 0 if key in ufuncs else 1
    cands.append((prio, fast[key]["hits"], m["name"]))
cands.sort()
names = [c[2] for c in cands[:maxn]]
json.dump(names, open(f"{out}/sample-{test}.json", "w"))
print(test, "eligible", len(cands), "sampled", len(names), "U-funcs-prio", sum(1 for c in cands[:maxn] if c[0] == 0))
