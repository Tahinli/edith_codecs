# usage: sample2.py OUT TEST N SEED -> OUT/s-TEST.json
# Seeded sample of mutants whose containing fast-suite function is in U(TEST) (the functions TEST
# covers that the fast suite does not). Writes the mutant names; prints eligible/sampled counts.
import json, random, sys, collections
out, test, n, seed = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])

def tail(p):
    i = p.find("ec-h265/")
    return p[i + 8:] if i >= 0 else None

def load(name):
    return json.load(open(f"{out}/{name}.json"))["data"][0]

def funcs(data):
    res = {}
    for fn in data["functions"]:
        if not fn["regions"]:
            continue
        fl = tail(fn["filenames"][fn["regions"][0][5]])
        if fl is None:
            continue
        lo = min(r[0] for r in fn["regions"])
        hi = max(r[2] for r in fn["regions"])
        key = (fl, fn["name"])
        d = res.setdefault(key, {"lo": lo, "hi": hi, "cov": False})
        d["lo"] = min(d["lo"], lo)
        d["hi"] = max(d["hi"], hi)
        d["cov"] = d["cov"] or any(r[4] > 0 for r in fn["regions"])
    return res

fast = funcs(load("fast"))
T = funcs(load(test))
U = {(x[0], x[1]) for x in json.load(open(f"{out}/coverage-summary.json"))["per_test"][test]["U_functions"]}
by_file = collections.defaultdict(list)
for key in fast:
    by_file[key[0]].append(key)

elig = []
covered = []
for m in json.load(open(f"{out}/mutants.json")):
    f = tail(m["file"]) if "ec-h265/" in m["file"] else m["file"]
    line = m["span"]["start"]["line"]
    best = None
    for key in by_file.get(f, []):
        fn = fast[key]
        if not T.get(key, {}).get("cov"):
            continue
        if fn["lo"] <= line <= fn["hi"]:
            if best is None or (fn["hi"] - fn["lo"]) < (best[1]["hi"] - best[1]["lo"]):
                best = (key, fn)
    if best is None:
        continue
    key = best[0]
    if key in U and T.get(key, {}).get("cov"):
        elig.append(m["name"])
    elif T.get(key, {}).get("cov"):
        covered.append(m["name"])

elig = sorted(set(elig))
covered = sorted(set(covered) - set(elig))
# Fallback when U(T) holds no source functions (e.g. only test-file functions): mutants in functions
# T covers, which the protocol's step 3 allows.
pool, label = (elig, "U") if elig else (covered, "covered-fallback")
picked = random.Random(seed).sample(pool, min(n, len(pool)))
json.dump(picked, open(f"{out}/s-{test}.json", "w"))
print(test, label, len(pool), "sampled", len(picked), "seed", seed)
