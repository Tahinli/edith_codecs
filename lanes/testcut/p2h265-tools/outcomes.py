# usage: outcomes.py OUTCOMES_JSON SUMMARY_KIND  (kind: missed | killed)  -> prints mutant names
import json, sys
data = json.load(open(sys.argv[1]))
kind = sys.argv[2]
want = {"missed": {"MissedMutant"}, "killed": {"CaughtMutant", "Timeout"}}[kind]
rows = data["outcomes"] if isinstance(data, dict) else data
for o in rows:
    sc = o.get("scenario")
    if not isinstance(sc, dict) or "Mutant" not in sc:
        continue
    if o.get("summary") in want:
        m = sc["Mutant"]
        print(m.get("name", json.dumps(m)))
