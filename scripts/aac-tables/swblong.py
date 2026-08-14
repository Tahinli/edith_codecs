"""Long-window scalefactor-band offsets, one sweep per samplingFrequencyIndex."""

import json

import swb

res = {}
for idx in range(12):
    edges = swb.long_offsets(idx)
    res[idx] = {"long": edges}
    print(idx, len(edges), edges, flush=True)
json.dump(res, open("swb.json", "w"))
