#!/usr/bin/env python3
"""Emit `default_filter_intra_cdfs` as (width, height, default) straight from
the libaom oracle source, so the ec-av1 test that pins those rows is never
transcribed by hand (class: shared-oracle blindness / table-indexed-by-raw).

Reads BLOCK_SIZES_ALL's order from av1/common/enums.h and the CDF table from
av1/common/entropymode.c, and prints the Rust literal rows for the shapes
`av1_filter_intra_allowed_bsize` allows (both sides <= 32).

    scripts/extract-filter-intra-cdfs.py [<aom source tree>]
"""
import re
import sys
from pathlib import Path

src = Path(sys.argv[1] if len(sys.argv) > 1 else Path.home() / ".cache/aom-oracle/src")

enums = (src / "av1/common/enums.h").read_text()
body = enums[enums.index("  BLOCK_4X4,"):enums.index("  BLOCK_SIZES_ALL,")]
sizes = [tuple(int(x) for x in m) for m in re.findall(r"BLOCK_(\d+)X(\d+),", body)]

ent = (src / "av1/common/entropymode.c").read_text()
start = ent.index("default_filter_intra_cdfs")
table = ent[start:ent.index("};", start)]
values = [int(v) for v in re.findall(r"AOM_CDF2\((\d+)\)", table)]

assert len(values) == len(sizes), f"{len(values)} rows vs {len(sizes)} block sizes"
for (w, h), v in zip(sizes, values):
    if w <= 32 and h <= 32:
        print(f"            ({w}, {h}, {v}),")
