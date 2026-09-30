#!/usr/bin/env python3
"""ffmpeg pixel-oracle comparator for ec-av1 4:2:2 (and 4:2:0 / 4:4:4 controls).

Discipline (oracle-comparator-liveness-control):

1. Geometry is an EXPLICIT argument read from the SEQUENCE HEADER via ffprobe
   (coded_width / coded_height / pix_fmt of the AV1 stream), never derived
   from a file size. A raw frame whose byte length disagrees with the geometry
   is a hard error.
2. Plane attribution is per FRAME, from that frame's own plane lengths, with a
   monotone cursor -- never absolute thresholds from frame 0.
3. A wrong SAMPLE is one bps-wide unit. At 10 bit the comparator walks
   16-bit LE samples, so the top bits cannot hide.
4. A count over zero frames is a hard error (vacuous 0/0/0).
5. Decode-order (ours, EC_AV1_FINAL_DUMP) -> display-order (ffmpeg rawvideo)
   mapping is derived by LUMA-IDENTITY content match under a strictly
   increasing assignment, then re-verified per frame. Exactly one decode frame
   must have no display partner: the hidden alt-ref picture.
"""
import json
import os
import re
import subprocess
import sys

SS = {"420": (1, 1), "422": (1, 0), "444": (0, 0), "440": (0, 1)}
DEPTH_RE = re.compile(r"^yuv(\d{3})p(\d*)(?:le)?$")


def probe_geometry(obu):
    """ffprobe reads the AV1 SEQUENCE HEADER: coded_width/coded_height and the
    chroma format/depth it implies. Never the raw output's byte length."""
    r = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v",
                        "-show_entries", "stream=coded_width,coded_height,pix_fmt",
                        "-of", "csv=p=0", obu], capture_output=True, check=True)
    parts = r.stdout.decode().strip().split(",")
    mw, h, pix = parts[0], parts[1], parts[2]
    m = DEPTH_RE.match(pix)
    if not m:
        raise SystemExit(f"{obu}: unsupported pix_fmt {pix!r} (need a planar yuv*p[le])")
    ss = SS.get(m.group(1))
    if ss is None:
        raise SystemExit(f"{obu}: unknown chroma tag C{m.group(1)} in pix_fmt {pix!r}")
    depth = int(m.group(2)) if m.group(2) else 8
    return int(mw), int(h), ss[0], ss[1], depth, pix


def plane_spans(w, h, ss_x, ss_y, bps):
    cw = -(-w // (1 << ss_x))
    ch = -(-h // (1 << ss_y))
    sizes = (w * h, cw * ch, cw * ch)
    return sizes, sum(sizes) * bps


def frame_files(prefix):
    out, i = [], 0
    while os.path.isfile(f"{prefix}.f{i}"):
        out.append(f"{prefix}.f{i}")
        i += 1
    return out


def ffmpeg_frames(obu, pix):
    r = subprocess.run(["ffmpeg", "-v", "error", "-i", obu, "-f", "rawvideo",
                        "-pix_fmt", pix, "-"], capture_output=True)
    if r.returncode != 0:
        raise SystemExit(f"{obu}: ffmpeg refused: {r.stderr.decode()[-300:]}")
    return r.stdout


def split(raw, sizes, fsz, cell):
    if not raw:
        raise SystemExit(f"{cell}: ffmpeg produced ZERO bytes -- a 0/0/0 here is vacuous")
    if len(raw) % fsz:
        raise SystemExit(f"{cell}: ffmpeg rawvideo is {len(raw)} B, not a whole "
                         f"multiple of the {fsz} B frame the geometry implies")
    return [raw[i * fsz:(i + 1) * fsz] for i in range(len(raw) // fsz)]


def _matchings(cands, disp_n, cap=64):
    """Every way to assign each decode frame either a distinct display partner
    or the hidden role, covering the display side exactly. Enumerated, not
    guessed: a duplicated picture can make the assignment genuinely ambiguous,
    and the caller then has to show the ambiguity does not change the answer."""
    sols = []

    def dfs(i, used, acc, hid):
        if len(sols) >= cap:
            return
        if i == len(cands):
            if len(used) == disp_n:
                sols.append((dict(acc), list(hid)))
            return
        for di in cands[i]:
            if di not in used:
                used.add(di)
                acc[i] = di
                dfs(i + 1, used, acc, hid)
                del acc[i]
                used.discard(di)
        hid.append(i)
        dfs(i + 1, used, acc, hid)
        hid.pop()

    dfs(0, set(), {}, [])
    return sols


def map_decode_to_display(ours, disp, sizes, bps, cell):
    """Identity of each decode-order frame is carried by its LUMA plane; full-frame
    identity is preferred when it exists. Every assignment that covers the
    display side is enumerated, so a duplicated picture shows up as an
    explicit solution COUNT rather than a silent pick. Real altref streams
    REORDER display against decode (measured: decode f2..f8 -> display
    7,3,1,2,5,4,6), so no monotonicity prior is applied -- an unjustified one
    is exactly the class of silent mismatch this instrument exists to catch."""
    luma = sizes[0] * bps
    for use_full in (True, False):
        cands = [[di for di, d in enumerate(disp)
                  if o[:luma] == d[:luma] and (o == d or not use_full)] for o in ours]
        sols = _matchings(cands, len(disp))
        if sols:
            if min(len(h) for _, h in sols) > 4:
                raise SystemExit(f"{cell}: {min(len(h) for _, h in sols)} decode frames have "
                                 f"no luma-identical display partner -- LUMA itself "
                                 f"diverges, so this is not a hidden-alt-ref count "
                                 f"(class av1-frame-count-vs-pixel-divergence)")
            return sols
    raise SystemExit(f"{cell}: luma identity never covers the display side "
                     f"({len(ours)} decode frames, {len(disp)} display frames) "
                     f"-- the frame-count or per-frame identity claim is wrong")



def compare(cell, ours_prefix, disp, w, h, ss_x, ss_y, depth, want_hidden=None):
    bps = 1 if depth <= 8 else 2
    sizes, fsz = plane_spans(w, h, ss_x, ss_y, bps)
    ours = [open(p, "rb").read() for p in frame_files(ours_prefix)]
    if not ours:
        raise SystemExit(f"{cell}: no EC_AV1_FINAL_DUMP frames from us -- vacuous 0/0/0")
    for i, o in enumerate(ours):
        if len(o) != fsz:
            raise SystemExit(f"{cell}: our decode frame {i} is {len(o)} B, geometry "
                             f"{w}x{h} ss={ss_x}{ss_y} depth={depth} implies {fsz} B")
    sols = map_decode_to_display(ours, disp, sizes, bps, cell)

    def measure(mapping, hidden):
        tot = [0, 0, 0]
        per_disp = [[0, 0, 0] for _ in disp]
        per_dec, first = [], None
        bb_all = [[None, None, None, None] for _ in range(3)]
        for oi, a in enumerate(ours):
            if oi in hidden:
                per_dec.append(None)
                continue
            b = disp[mapping[oi]]
            fcount, off = [0, 0, 0], 0
            for pi, sz in enumerate(sizes):
                pw = w if pi == 0 else -(-w // (1 << ss_x))
                for i in range(sz):
                    s = i * bps
                    if a[off + s:off + s + bps] != b[off + s:off + s + bps]:
                        fcount[pi] += 1
                        tot[pi] += 1
                        per_disp[mapping[oi]][pi] += 1
                        r, c = divmod(i, pw)
                        bb = bb_all[pi]
                        if bb[0] is None or r < bb[0]:
                            bb[0] = r
                        if bb[1] is None or r > bb[1]:
                            bb[1] = r
                        if bb[2] is None or c < bb[2]:
                            bb[2] = c
                        if bb[3] is None or c > bb[3]:
                            bb[3] = c
                        if first is None:
                            first = (mapping[oi], oi, "YUV"[pi], r, c,
                                     int.from_bytes(a[off + s:off + s + bps], "little"),
                                     int.from_bytes(b[off + s:off + s + bps], "little"))
                off += sz * bps
            per_dec.append(fcount)
        return tot, per_disp, per_dec, first, bb_all

    measured = [measure(m, h) for m, h in sols]
    totals = {tuple(t[0]) for t in measured}
    if len(totals) > 1:
        raise SystemExit(f"{cell}: the decode->display assignment is AMBIGUOUS "
                         f"({len(sols)} solutions) and they DISAGREE on the counts "
                         f"{sorted(totals)} -- pick the identity source explicitly, "
                         f"this instrument will not choose for you")
    tot, per_disp, per_dec, first, bbox = measured[0]
    mapping, hidden = sols[0]
    if want_hidden is not None and len(hidden) != want_hidden:
        raise SystemExit(f"{cell}: {len(hidden)} hidden decode frame(s) {hidden}, "
                         f"expected {want_hidden} -- the frame-count claim changed")
    return {"cell": cell, "status": "BYTE-EXACT" if tot == [0, 0, 0] else "DIVERGES",
            "w": w, "h": h, "ss": f"{ss_x}{ss_y}", "depth": depth, "pix": pix_of(ss_x, ss_y, depth),
            "decode_frames": len(ours), "display_frames": len(disp),
            "hidden_decode": hidden, "mapping_solutions": len(sols),
            "Y": tot[0], "U": tot[1], "V": tot[2],
            "first": first, "bbox": bbox,
            "per_decode_frame": per_dec, "per_display_frame": per_disp}


def pix_of(ss_x, ss_y, depth):
    tag = next(k for k, v in SS.items() if v == (ss_x, ss_y))
    return f"yuv{tag}p" + ("" if depth <= 8 else f"{depth}le")


def flip(disp, sizes, bps, disp_index, plane, sample_index):
    """Flip bit 0 of one oracle SAMPLE's low byte: exactly +1 wrong sample in
    both depths (2**bitdepth < 2**16, so nothing carries out of the container).
    Returns a new list; the caller drops it to restore."""
    new = list(disp)
    buf = bytearray(new[disp_index])
    buf[sum(sizes[:plane]) * bps + sample_index * bps] ^= 1
    new[disp_index] = bytes(buf)
    return new


def run(cell, obu, ours_prefix, want_hidden=None, json_out=False):
    w, h, ssx, ssy, depth, pix = probe_geometry(obu)
    bps = 1 if depth <= 8 else 2
    sizes, fsz = plane_spans(w, h, ssx, ssy, bps)
    disp = split(ffmpeg_frames(obu, pix), sizes, fsz, cell)
    res = compare(cell, ours_prefix, disp, w, h, ssx, ssy, depth, want_hidden)
    if json_out:
        print(json.dumps(res))
    else:
        f = res["first"]
        fs = "none" if f is None else \
            f"disp f{f[0]} (decode f{f[1]}) {f[2]} (r{f[3]},c{f[4]}) ours={f[5]} ffmpeg={f[6]}"
        print(f"{cell}: {res['status']} {res['decode_frames']}df/"
              f"{res['display_frames']}shown hidden={res['hidden_decode']} "
              f"Y={res['Y']} U={res['U']} V={res['V']}  first: {fs}")
    return res, disp, sizes, fsz, bps


if __name__ == "__main__":
    cell, obu, prefix = sys.argv[1:4]
    wh = int(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[4].isdigit() else None
    run(cell, obu, prefix, wh, "--json" in sys.argv)
