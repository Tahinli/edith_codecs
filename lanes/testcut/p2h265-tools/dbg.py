import json, os, sys
sys.argv = ["x", os.path.expanduser("~/lanes/testcut-p2-h265-c1/out"), "pixel_pipeline_runs_over_conformance_streams", "1"]
src = open(os.path.join(os.getcwd(), "lanes/testcut/p2h265-tools/sample.py")).read().split("cands = []")[0]
exec(src)
muts = json.load(open(f"{out}/mutants.json"))
nf = sum(1 for m in muts if tail(m["file"]) in {k[0] for k in fast})
print("mutants", len(muts), "file-matched", nf, "fast funcs", len(fast), "sample files", list({k[0] for k in fast})[:3], "mut files", list({tail(m["file"]) for m in muts})[:3])
found = inT = 0
for m in muts:
    f = tail(m["file"]); line = m["span"]["start"]["line"]
    hits = [fn for k, fn in fast.items() if k[0] == f and fn["lo"] <= line <= fn["hi"]]
    if hits:
        found += 1
        b = min(hits, key=lambda fn: fn["hi"] - fn["lo"])
        t = T.get((b["file"], b["name"]))
        if t and t["covered_regions"]:
            inT += 1
print("found", found, "inT", inT, "Tfuncs", len(T), "sample T keys", list(T)[:2])
print("T funcs with covered", sum(1 for f in T.values() if f["covered_regions"]), "fast hits>0", sum(1 for f in fast.values() if f["hits"]>0))
import itertools
print("sample covered T:", [ (k[1][:60], len(v["covered_regions"])) for k,v in itertools.islice(((k,v) for k,v in T.items() if v["covered_regions"]),3)])
cnt = 0
for m in muts[:4000]:
    f = tail(m["file"]); line = m["span"]["start"]["line"]
    hits = [fn for k, fn in fast.items() if k[0] == f and fn["lo"] <= line <= fn["hi"]]
    if hits:
        b = min(hits, key=lambda fn: fn["hi"] - fn["lo"])
        t = T.get((b["file"], b["name"]))
        if cnt < 3:
            print("MUT", f, line, "fn", b["name"][-40:], "inT", t is not None, "covered", len(t["covered_regions"]) if t else None, "fast hits", b["hits"])
        cnt += 1
        if cnt >= 3: break
print("Tcovered files", sorted({k[0] for k,v in T.items() if v["covered_regions"]})[:8])
