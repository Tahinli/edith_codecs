#!/usr/bin/env python3
"""Flip control: prove the ffmpeg comparator BITES, before any verdict.

[`cmp422-ffmpeg`][c] re-derives the decode->display assignment from LUMA
identity on every call, so a flipped oracle sample can never be re-mapped --
the identity it keys on is exactly what the flip breaks. This control
therefore maps ONCE from the untouched streams and then flips one SAMPLE per
arm with the mapping PINNED, which is the only way to ask "does one wrong
sample move the count by exactly one?".

Two independent directions, both required to pass:

* **oracle arm** -- flip one ffmpeg sample. The plane that owns it must move by
  EXACTLY +1, every other plane by exactly 0. On a byte-exact cell this is the
  only thing that can move, so a `0/0/0` that survives a flip is a comparator
  that is not reading the oracle.
* **ours arm** -- flip one of OUR samples with the oracle untouched. Same
  delta. This is the direction the whole corpus verdict rests on, and on a
  `0/0/0` cell it is what separates "we agree" from "we never looked".

Every decode->display SOLUTION is exercised, so an ambiguous mapping cannot
hide a failure behind the one that was picked.

Usage:
    python3 scripts/flipctl-422.py cells.json

`cells.json` entries: `{"name", "path", "prefix"}`, `prefix` being our
`EC_AV1_FINAL_DUMP` prefix. Exit status 0 only if every arm passes.
"""
import importlib.util
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "cmp422_ffmpeg", os.path.join(_HERE, "cmp422-ffmpeg.py"))
C = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(C)


def plane_counts(ours, disp, mapping, hidden, sizes, bps):
    """The comparator's own accounting (`cmp422-ffmpeg.compare`'s `measure`),
    with the decode->display assignment PINNED so a flipped sample cannot
    re-map."""
    tot = [0, 0, 0]
    for oi, a in enumerate(ours):
        if oi in hidden:
            continue
        b = disp[mapping[oi]]
        off = 0
        for pi, sz in enumerate(sizes):
            for i in range(sz):
                s = off + i * bps
                if a[s:s + bps] != b[s:s + bps]:
                    tot[pi] += 1
            off += sz * bps
    return tot


def flip_sample(buf, sample_index, bps):
    """Flip bit 0 of one SAMPLE's low byte: exactly +1 wrong sample at both
    depths (2**bitdepth < 2**16, so nothing carries out of the container)."""
    out = bytearray(buf)
    out[sample_index * bps] ^= 1
    return bytes(out)


def arms_for(sizes):
    """(plane, sample_index) per plane: the first, the middle and the last."""
    out = []
    for pi, sz in enumerate(sizes):
        out += [(pi, 0), (pi, sz // 2), (pi, sz - 1)]
    return out


def one_cell(cell, path, prefix):
    w, h, ssx, ssy, depth, pix = C.probe_geometry(path)
    bps = 1 if depth <= 8 else 2
    sizes, fsz = C.plane_spans(w, h, ssx, ssy, bps)
    disp = C.split(C.ffmpeg_frames(path, pix), sizes, fsz, cell)
    ours = [open(p, "rb").read() for p in C.frame_files(prefix)]
    if not ours:
        raise SystemExit(f"{cell}: no EC_AV1_FINAL_DUMP frames -- vacuous")
    for i, o in enumerate(ours):
        if len(o) != fsz:
            raise SystemExit(f"{cell}: our frame {i} is {len(o)} B, "
                             f"geometry {w}x{h} ss={ssx}{ssy} depth={depth} implies {fsz} B")
    sols = C.map_decode_to_display(ours, disp, sizes, bps, cell)
    shown = set()
    for mapping, hidden in sols:
        shown |= {d for oi, d in enumerate(mapping) if oi not in hidden}
    shown = sorted(shown)
    # Two shown display frames per cell -- the first and one in the middle --
    # so the arm is not confined to frame 0.
    ks = sorted({shown[0], shown[len(shown) // 2]})
    results, arms = [], 0
    base0 = plane_counts(ours, disp, sols[0][0], sols[0][1], sizes, bps)
    for k in ks:
        for sol_i, (mapping, hidden) in enumerate(sols):
            dmap = {d: oi for oi, d in enumerate(mapping) if oi not in hidden}
            if k not in dmap:
                continue
            oi = dmap[k]
            base = plane_counts(ours, disp, mapping, hidden, sizes, bps)
            for pi, si in arms_for(sizes):
                off = sum(sizes[:pi]) + si          # sample index inside the frame
                want = [0, 0, 0]
                want[pi] = 1
                for side in ("oracle", "ours"):
                    if side == "oracle":
                        f = list(disp)
                        f[k] = flip_sample(disp[k], off, bps)
                        got = plane_counts(ours, f, mapping, hidden, sizes, bps)
                    else:
                        f = list(ours)
                        f[oi] = flip_sample(ours[oi], off, bps)
                        got = plane_counts(f, disp, mapping, hidden, sizes, bps)
                    delta = [got[i] - base[i] for i in range(3)]
                    ok = delta == want
                    results.append(ok)
                    arms += 1
                    if not ok:
                        print(f"  FAIL {side:6s} disp f{k} plane {'YUV'[pi]} "
                              f"sample {si} sol{sol_i}: {base} -> {got} "
                              f"delta {delta} want {want}")
    print(f"{cell}: baseline {base0}  {len(ours)} decode / {len(disp)} shown, "
          f"hidden {sols[0][1]}, {len(sols)} mapping solution(s), {arms} arms")
    return results


def main(spec_path):
    rows = []
    for c in json.load(open(spec_path)):
        rows += one_cell(c["name"], c["path"], c["prefix"])
    print(f"\n{sum(rows)} PASS, {len(rows) - sum(rows)} FAIL of {len(rows)} flip arms")
    return 0 if all(rows) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))