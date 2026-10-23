# Usage: killed.py <mutants.out dir> ; prints JSON {mutant_name: {"outcome": ..., "fails": [tests]}}
# Reads mutants.out/outcomes.json (cargo-mutants) and mutants.out/log/*.log (nextest FAIL lines).
import json, os, re, sys
d = sys.argv[1]
res = {}
oc = json.load(open(os.path.join(d, 'outcomes.json')))
for o in oc.get('outcomes', []):
    s = o.get('scenario')
    if not isinstance(s, dict) or 'Mutant' not in s:
        continue
    name = s['Mutant'].get('name')
    res[name] = {'outcome': o.get('summary'), 'fails': []}
fail_re = re.compile(r'FAIL \[[^\]]*\]\s+(\S+)\s+(\S+)')
logdir = os.path.join(d, 'log')
for f in os.listdir(logdir) if os.path.isdir(logdir) else []:
    txt = open(os.path.join(logdir, f), errors='replace').read()
    fails = set(m.group(2) for m in fail_re.finditer(txt))
    # mutant name is recorded in the log's first line
    first = txt.split('\n', 1)[0]
    for name in res:
        if name in first or name.replace(' ', '') in first.replace(' ', ''):
            res[name]['fails'] = sorted(fails)
print(json.dumps(res, indent=1))
