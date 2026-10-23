# Usage: regions.py <out-dir> <T-name> ; prints JSON {U: [...], covered_T: n, sets} for U(T) analysis.
# Region = [LS,CS,LE,CE,count,fileId,expanded,kind] per function entry in llvm-cov JSON.
import json, sys, os
def load(p):
    d = json.load(open(p))['data'][0]
    cnt = {}
    for f in d['functions']:
        for r in f['regions']:
            fn = f['filenames'][r[5]]
            key = (fn, r[0], r[1], r[2], r[3])
            cnt[key] = max(cnt.get(key, 0), r[4])
    return cnt
out, t = sys.argv[1], sys.argv[2]
T = load(os.path.join(out, t + '.json'))
F = load(os.path.join(out, 'fast.json'))
covT = {k for k, v in T.items() if v > 0}
U = sorted(k for k in covT if F.get(k, 0) == 0)
json.dump({'covered_T': len(covT), 'U_count': len(U),
           'U': [[k[0], k[1], k[3]] for k in U],
           'covered': [[k[0], k[1], k[3]] for k in covT]},
          open(os.path.join(out, t + '.regions.json'), 'w'))
print(json.dumps({'test': t, 'covered_T_regions': len(covT), 'U_regions': len(U)}))
