#!/usr/bin/env python3
"""Driver: decode a cell with our decode_probe (EC_AV1_FINAL_DUMP) and compare
against ffmpeg, per plane, per frame. Geometry from the sequence header."""
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cmpff  # noqa: E402

PROBE = "/home/tahinli/.cache/tgt/av1422seed/debug/examples/decode_probe"
WORK = "/home/tahinli/.cache/seed422"


def decode_ours(name, path, extra_env=None):
    d = os.path.join(WORK, name)
    os.makedirs(d, exist_ok=True)
    for f in os.listdir(d):
        os.remove(os.path.join(d, f))
    prefix = os.path.join(d, "ours")
    env = dict(os.environ, EC_AV1_FINAL_DUMP=prefix)
    env.update(extra_env or {})
    r = subprocess.run([PROBE, path], stdout=subprocess.PIPE,
                       stderr=subprocess.STDOUT, timeout=3600, env=env)
    out = r.stdout.decode("utf-8", "replace")
    if "REFUSED:" in out:
        return None, out.split("REFUSED:", 1)[1].strip().splitlines()[0]
    return prefix, out


def one(name, path, want_hidden=None, keep=True):
    prefix, out = decode_ours(name, path)
    if prefix is None:
        print(f"{name}: REFUSES: {out}")
        return {"cell": name, "status": "REFUSES", "msg": out}
    res, *_ = cmpff.run(name, path, prefix, want_hidden, json_out=False)
    res["probe"] = out.strip().splitlines()[-1] if out.strip() else ""
    return res


if __name__ == "__main__":
    cells = json.load(open(sys.argv[1]))
    out = []
    for c in cells:
        try:
            res = one(c["name"], c["path"], c.get("hidden"))
        except SystemExit as e:
            res = {"cell": c["name"], "status": "COMPARATOR-ERROR", "msg": str(e)}
        print(json.dumps({k: v for k, v in res.items()
                          if k not in ("per_decode_frame", "per_display_frame", "bbox")}),
              flush=True)
        out.append(res)
    json.dump(out, open(sys.argv[2], "w"), indent=1)
