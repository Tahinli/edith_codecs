import json, sys, collections
out, names = sys.argv[1], sys.argv[2:]
def load(n):
    return json.load(open(f"{out}/{n}.json"))["data"][0]
def regions(data):
    s = {}
    for fn in data["functions"]:
        for r in fn["regions"]:
            fname = fn["filenames"][r[5]]
            key = (fn["name"], fname, r[0], r[1], r[2], r[3], r[7])
            s[key] = max(s.get(key, 0), r[4])
    return s
fast = load("fast")
fr = regions(fast)
fast_cov = {k for k, v in fr.items() if v > 0}
res = {}
for n in names:
    d = load(n)
    r = regions(d)
    cov = {k for k, v in r.items() if v > 0}
    u = cov - fast_cov
    fns = collections.Counter()
    for k in u:
        fi = k[1].find("ec-h265/"); fns[(k[1][fi + 8:] if fi >= 0 else None, k[0])] += 1
    res[n] = {"covered": len(cov), "U": len(u), "U_functions": [[f, fn, c] for (f, fn), c in fns.items() if f]}
    print(n, "covered", len(cov), "U", len(u), "funcs", len(fns))
json.dump({"fast_regions": len(fast_cov), "per_test": res}, open(f"{out}/coverage-summary.json", "w"))
