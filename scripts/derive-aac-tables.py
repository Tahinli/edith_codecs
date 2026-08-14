#!/usr/bin/env python3
"""Derive the normative AAC-LC tables by black-box probing of a reference decoder.

Clean room: no AAC implementation source is read.  The only oracle is ffmpeg's
`aac` decoder, driven as a black box.  Synthetic access units carry a spectral
payload we choose; decoding them and inverting the MDCT recovers the quantised
values, and a binary-tree walk over bit prefixes enumerates each codebook.

A bit string `b` is a complete codeword exactly when the first decoded tuple does
not change if a `0` is appended -- prefix codes are injective, and the filler
after the codeword is identical in both probes.  That makes the walk about `2M`
probes for `M` codewords, and every codebook it produces is checked against a
Kraft sum of exactly 1 and the entry count ISO/IEC 14496-3 prescribes, which is
what makes the derivation self-checking.

The band tables come out the same way: with `max_sfb = k` the highest non-zero
coefficient is `swb_offset[k] - 1`, and the TNS band limits show up as the
`max_sfb` at which a filter stops being applied.

Usage:  python3 scripts/derive-aac-tables.py [work-dir]
Writes cb1..cb11.json, sfb.json, swb.json and swb_short.json into the work
directory, then regenerates crates/ec-aac/src/tables.rs.  Needs numpy and ffmpeg.
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
MODULES = os.path.join(HERE, "aac-tables")
CODEBOOKS = [
    (1, 4, "s"), (2, 4, "s"), (3, 4, "u"), (4, 4, "u"), (5, 2, "s"), (6, 2, "s"),
    (7, 2, "u"), (8, 2, "u"), (9, 2, "u"), (10, 2, "u"),
]


def run(args, cwd):
    print("+", " ".join(args), flush=True)
    subprocess.run([sys.executable, *args], cwd=cwd, check=True)


def main():
    work = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "aac-tables-work")
    os.makedirs(work, exist_ok=True)
    for name in os.listdir(MODULES):
        if name.endswith(".py"):
            with open(os.path.join(MODULES, name)) as src, \
                 open(os.path.join(work, name), "w") as dst:
                dst.write(src.read())
    for cb, dim, signed in CODEBOOKS:
        run(["learn.py", str(cb), str(dim), signed], work)
    run(["learn11.py", "11"], work)
    run(["learn11.py", "sf"], work)
    run(["swblong.py"], work) if os.path.exists(os.path.join(work, "swblong.py")) else None
    run(["swbshort.py"], work)
    out = os.path.join(HERE, "..", "crates", "ec-aac", "src", "tables.rs")
    run(["gen_tables.py", os.path.abspath(out)], work)
    with open(os.path.join(work, "cb11.json")) as f:
        print("cb11 entries:", len(json.load(f)))


if __name__ == "__main__":
    main()
