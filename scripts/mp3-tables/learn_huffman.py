"""Measure every Layer III Huffman table by walking the code tree against the
ffmpeg decoder: a prefix is a codeword exactly when every continuation decodes
to the same pair (values clamped at 15, so linbits escapes compare equal)."""

import json
import sys
import numpy as np
from probe_engine import Engine, values

# (table index used on the wire, entries per axis, linbits, coefficients read)
BIG_TABLES = [
    (1, 2, 0), (2, 3, 0), (3, 3, 0), (5, 4, 0), (6, 4, 0),
    (7, 6, 0), (8, 6, 0), (9, 6, 0), (10, 8, 0), (11, 8, 0), (12, 8, 0),
    (13, 16, 0), (15, 16, 0), (16, 16, 1), (24, 16, 4),
]
MAXDEPTH = 24


def learn_table(engine, tsel, dim, quad=False):
    """Returns {codeword bits (tuple) : tuple of values}."""
    leaves = {}
    level = [()]
    depth = 0
    while level:
        depth += 1
        if depth > MAXDEPTH:
            raise RuntimeError(f"table {tsel}: tree deeper than {MAXDEPTH}")
        probes = []
        for prefix in level:
            for fill in (0, 1):
                bits = list(prefix) + [fill] * 48
                if quad:
                    probes.append((bits, 0, 0, tsel))
                else:
                    probes.append((bits, tsel, 1, 0))
        got = engine.run(probes)
        n = 4 if quad else 2
        nxt = []
        for i, prefix in enumerate(level):
            v0 = np.minimum(values(got[2 * i], n), 15)
            v1 = np.minimum(values(got[2 * i + 1], n), 15)
            if tuple(v0) == tuple(v1):
                leaves[prefix] = tuple(int(x) for x in v0)
            else:
                nxt.append(prefix + (0,))
                nxt.append(prefix + (1,))
        level = nxt
    return leaves


def check(leaves, dim, quad, name):
    kraft = sum(2.0 ** -len(c) for c in leaves)
    vals = sorted(leaves.values())
    expect = dim ** (4 if quad else 2)
    ok = True
    if abs(kraft - 1.0) > 1e-12:
        print(f"  !! {name}: kraft = {kraft}")
        ok = False
    if len(leaves) != expect:
        print(f"  !! {name}: {len(leaves)} leaves, expected {expect}")
        ok = False
    if len(set(vals)) != len(vals):
        print(f"  !! {name}: duplicate values")
        ok = False
    return ok


def main():
    Minv = np.load("Minv.npy")
    engine = Engine(Minv)
    out = {}
    allok = True
    for tsel, dim, linbits in BIG_TABLES:
        leaves = learn_table(engine, tsel, dim)
        ok = check(leaves, dim, False, f"table {tsel}")
        allok &= ok
        maxlen = max(len(c) for c in leaves)
        print(f"table {tsel:2d}: {len(leaves):3d} codes, max len {maxlen:2d}, "
              f"linbits {linbits}, ok={ok}")
        out[str(tsel)] = {"".join(map(str, c)): v for c, v in leaves.items()}
    for tsel in (0, 1):
        leaves = learn_table(engine, tsel, 2, quad=True)
        ok = check(leaves, 2, True, f"count1 {tsel}")
        allok &= ok
        print(f"count1 {tsel}: {len(leaves)} codes, max len "
              f"{max(len(c) for c in leaves)}, ok={ok}")
        out[f"count1_{tsel}"] = {"".join(map(str, c)): v for c, v in leaves.items()}
    with open("huffman.json", "w") as f:
        json.dump(out, f, indent=0)
    print("all tables consistent:", allok)
    return 0 if allok else 1


if __name__ == "__main__":
    sys.exit(main())
