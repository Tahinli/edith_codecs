#!/usr/bin/env python3
"""Extract libaom's eob_multi CDF rows and emit / verify our Rust consts.

`av1_default_eob_multi{16,32,64,128,256,512,1024}_cdfs` are
`[TOKEN_CDF_Q_CTXS][PLANE_TYPES][2][CDF_SIZE(n)]`; the third index is
`eob_multi_ctx = (tx_class == TX_CLASS_2D) ? 0 : 1`.  This prints the
class-1 (index 1) rows as `EOB_PT_<n>_<PLANE>_CLASS1[_Qk]` consts and, with
`--check <cdf.rs>`, diffs every const that file already defines -- BOTH classes,
so a 2D const accidentally transcribed from the class-1 row is caught too
(rectsplit r4 suspected exactly that of `EOB_PT_512_CHROMA`).

Usage: extract-eob-class1.py <token_cdfs.h> [--check <cdf.rs>]
"""

import re
import sys

SIZES = [16, 32, 64, 128, 256, 512, 1024]
QSUF = ["_Q0", "_Q1", "", "_Q3"]


def parse(src: str, size: int):
    m = re.search(rf"av1_default_eob_multi{size}_cdfs\b(.*?)\n\s*\n", src, re.S)
    if not m:
        sys.exit(f"table eob_multi{size} not found")
    rows = [
        [int(x) for x in r.split(",")]
        for r in re.findall(r"AOM_CDF\d+\(([^)]*)\)", m.group(1))
    ]
    # flat order: q-major, then plane, then class
    if len(rows) != 4 * 2 * 2:
        sys.exit(f"eob_multi{size}: expected 16 rows, got {len(rows)}")
    return rows


def consts(src):
    out = {}
    for size in SIZES:
        rows = parse(src, size)
        for q in range(4):
            for p, plane in enumerate(("LUMA", "CHROMA")):
                for cls, suf in ((0, ""), (1, "_CLASS1")):
                    row = rows[q * 4 + p * 2 + cls]
                    out[f"EOB_PT_{size}_{plane}{suf}{QSUF[q]}"] = row + [32768, 0]
    return out


def main():
    src = open(sys.argv[1]).read()
    want = consts(src)
    if "--check" in sys.argv:
        rs = open(sys.argv[sys.argv.index("--check") + 1]).read()
        seen = bad = 0
        for name, vals in want.items():
            m = re.search(rf"const {name}: \[u16; \d+\] =\s*(\[[^]]*\]);", rs)
            if not m:
                continue
            seen += 1
            got = [int(x) for x in re.findall(r"\d+", m.group(1))]
            if got != vals:
                bad += 1
                print(f"MISMATCH {name}\n  ours {got}\n  aom  {vals}")
        print(f"checked {seen}/{len(want)} consts, {bad} mismatched")
        missing = [n for n in want if f"const {n}:" not in rs]
        print(f"missing {len(missing)}: {' '.join(sorted(missing))}")
        sys.exit(1 if bad else 0)
    for name, vals in want.items():
        body = ", ".join(str(v) for v in vals)
        print(f"pub const {name}: [u16; {len(vals)}] = [{body}];")


main()
