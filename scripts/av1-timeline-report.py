#!/usr/bin/env python3
"""Reads `EC_AV1_TIMELINE=1` TSV (stderr of decode_probe) and reports what
bounds the wall: per-phase durations, reference waits, the critical path
through the reference DAG, concurrency over time, and dispatcher cap waits.

Usage: EC_AV1_TIMELINE=1 decode_probe f.obu 2>tl.tsv; av1-timeline-report.py tl.tsv
"""
import sys
from statistics import median

COLS = ("idx type show submit start refs_ready hdr_done tile_done recon_done "
        "filters_done slot_fulfil grain_done output wait_slot wait_owner wait_us "
        "pipe deps").split()
INT = set(COLS) - {"type", "deps"}

# (label, from, to) -- the phases of one frame, in order.
PHASES = [
    ("queue    submit->start", "submit", "start"),
    ("refwait  start->refs", "start", "refs_ready"),
    ("setup    refs->hdr", "refs_ready", "hdr_done"),
    ("parse    hdr->tile", "hdr_done", "tile_done"),
    ("recontail tile->recon", "tile_done", "recon_done"),
    ("filters  recon->filt", "recon_done", "filters_done"),
    ("publish  filt->fulfil", "filters_done", "slot_fulfil"),
    ("grain    fulfil->grain", "slot_fulfil", "grain_done"),
    ("outwait  grain->output", "grain_done", "output"),
]
# Which phase a chain segment's time is charged to (same order).
CHAIN_KEYS = [p[0].split()[0] for p in PHASES]


def load(path):
    frames, caps, end = [], [], 0
    with (open(path) if path != "-" else sys.stdin) as fh:
        for line in fh:
            f = line.rstrip("\n").split("\t")
            if f[0] == "TL":
                r = dict(zip(COLS, f[1:]))
                for k in INT:
                    r[k] = int(r[k])
                r["deps"] = [] if r["deps"] == "-" else [int(x) for x in r["deps"].split(",")]
                frames.append(r)
            elif f[0] == "TL_CAP":
                caps.append((f[1], int(f[2]), int(f[3])))
            elif f[0] == "TL_END":
                end = int(f[1])
    return {f["idx"]: f for f in frames}, caps, end


def ms(us):
    return us / 1000.0


def stats(vals):
    vals = sorted(v for v in vals if v is not None)
    if not vals:
        return None
    p90 = vals[min(len(vals) - 1, int(round(0.9 * (len(vals) - 1))))]
    return median(vals), p90, vals[-1], sum(vals), len(vals)


def dur(r, a, b):
    """Duration of one phase, or None when either end was never reached."""
    if r[a] == 0 or r[b] == 0 or r[b] < r[a]:
        return None
    return r[b] - r[a]


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "-"
    fr, caps, end = load(path)
    if not fr:
        sys.exit("no TL rows")
    t0 = min(min(v for k, v in r.items() if k in INT and k not in
                 ("idx", "show", "wait_slot", "wait_owner", "wait_us", "pipe") and v > 0)
             for r in fr.values())
    wall = end - t0
    n = len(fr)
    npipe = sum(r["pipe"] for r in fr.values())
    print(f"frames={n} (key={sum(1 for r in fr.values() if r['type']=='K')} "
          f"shown={sum(r['show'] for r in fr.values())} pipelined_filters={npipe}/{n}) "
          f"wall={ms(wall):.1f} ms  (first event {ms(t0):.1f} ms, end {ms(end):.1f} ms)")

    print("\n(a) per-frame phase durations, ms")
    print(f"{'phase':24} {'n':>4} {'median':>8} {'p90':>8} {'max':>8} {'sum':>9} {'sum/wall':>8}")
    for label, a, b in PHASES:
        s = stats([dur(r, a, b) for r in fr.values()])
        if not s:
            print(f"{label:24}    -")
            continue
        med, p90, mx, tot, cnt = s
        print(f"{label:24} {cnt:>4} {ms(med):>8.2f} {ms(p90):>8.2f} {ms(mx):>8.2f} "
              f"{ms(tot):>9.1f} {tot/wall:>8.2f}")
    s = stats([dur(r, "start", "grain_done") for r in fr.values()])
    if s:
        print(f"{'TOTAL    start->grain':24} {s[4]:>4} {ms(s[0]):>8.2f} {ms(s[1]):>8.2f} "
              f"{ms(s[2]):>8.2f} {ms(s[3]):>9.1f} {s[3]/wall:>8.2f}")

    print("\n(b) reference wait (start -> refs_ready), ms")
    waits = [(dur(r, "start", "refs_ready") or 0, r) for r in fr.values() if r["start"]]
    s = stats([w for w, _ in waits])
    print(f"    n={s[4]} median={ms(s[0]):.2f} p90={ms(s[1]):.2f} max={ms(s[2]):.2f} "
          f"sum={ms(s[3]):.1f} ({s[3]/wall:.2f}x wall)")
    zero = sum(1 for w, _ in waits if w < 1000)
    print(f"    frames waiting <1 ms: {zero}/{len(waits)}")
    print("    top waiters (frame -> longest-blocking producer):")
    for w, r in sorted(waits, key=lambda x: -x[0])[:10]:
        print(f"      f{r['idx']:<4}{r['type']} waited {ms(w):8.2f} on slot {r['wait_slot']} "
              f"produced by f{r['wait_owner']} (deps {r['deps']})")

    # (c) critical path. A frame's critical predecessor is the dependency
    # whose slot publish is the LAST one -- the edge that actually released
    # this frame's refs_ready.
    def pred(r):
        best, bt = None, -1
        for d in r["deps"]:
            p = fr.get(d)
            if p is None:
                continue
            t = own_w[d]
            if t > bt:
                best, bt = p, t
        return best

    # Walk back from the frame on the longest WORK-WEIGHTED chain (the frame
    # that finishes last is often in a short last-GOP chain: a key frame
    # resets the DAG, so wall-clock-last is not dependency-deepest).
    own_w = {}
    for i in sorted(fr):
        r = fr[i]
        own_w[i] = max([own_w.get(d, 0) for d in r["deps"]] + [0]) + sum(
            dur(r, a2, b2) or 0 for (_, a2, b2) in PHASES[2:7])
    tail = fr[max(own_w, key=lambda k: own_w[k])]
    chain, cur = [], tail
    while cur is not None:
        chain.append(cur)
        cur = pred(cur)
    chain.reverse()
    head = chain[0]
    span_a = head["hdr_done"] or head["start"] or t0
    span_b = max(tail["output"], tail["grain_done"])
    print(f"\n(c) critical path: {len(chain)} frames "
          f"({chain[0]['idx']} -> {chain[-1]['idx']}), "
          f"{ms(span_b - span_a):.1f} ms = {(span_b-span_a)/wall*100:.1f}% of wall")
    charge = dict.fromkeys(CHAIN_KEYS, 0)
    slack = 0
    for i, r in enumerate(chain):
        for (label, a, b) in PHASES:
            d = dur(r, a, b)
            if d is None:
                continue
            k = label.split()[0]
            if i == 0 and k in ("queue", "refwait"):
                continue
            # A frame's own wait is charged only for the part after its
            # predecessor published -- before that the predecessor's own
            # phases already account for the time.
            if k == "refwait":
                p = chain[i - 1]
                d = max(0, r["refs_ready"] - max(r["start"], p["slot_fulfil"] or p["filters_done"]))
            if k == "outwait" and r is not tail:
                continue
            charge[k] += d
        if i:
            p = chain[i - 1]
            pt = p["slot_fulfil"] or p["filters_done"]
            if r["start"] and r["start"] > pt:
                slack += r["start"] - pt
    print(f"    {'segment':12} {'ms':>9} {'% of chain':>11}")
    tot = span_b - span_a
    for k in CHAIN_KEYS:
        if charge[k]:
            print(f"    {k:12} {ms(charge[k]):>9.1f} {charge[k]/tot*100:>10.1f}%")
    print(f"    {'(late start)':12} {ms(slack):>9.1f} {slack/tot*100:>10.1f}%   "
          "producer published before the consumer's worker even started")
    print(f"    chain frames: {[r['idx'] for r in chain][:40]}"
          f"{' ...' if len(chain) > 40 else ''}")

    # Infinite-core lower bound: earliest finish of every frame if a core
    # were always free -- own work only, no queueing.
    own = {}
    for i in sorted(fr):
        r = fr[i]
        w = sum(dur(r, a2, b2) or 0 for (_, a2, b2) in PHASES[2:7])
        own[i] = max([own.get(d, 0) for d in r["deps"]] + [0]) + w
    crit = max(own, key=lambda k: own[k])
    print(f"    infinite-core bound (longest work-weighted chain, ends at f{crit}): "
          f"{ms(own[crit]):.1f} ms = {own[crit]/wall*100:.1f}% of wall")
    work = sum(sum(dur(r, a2, b2) or 0 for (_, a2, b2) in PHASES[2:7]) for r in fr.values())
    print(f"    frame-thread work total {ms(work):.1f} ms = {work/wall:.2f} cores' worth; "
          f"work/bound = {work/own[crit]:.2f} (the parallelism the DAG would need)")

    print("\n(d) concurrency (frames between start and slot publish), 10 ms ticks")
    def tick(live, label):
        counts = []
        t = t0
        while t < end:
            counts.append(sum(1 for a, b in live if a <= t < b))
            t += 10000
        counts.sort()
        lt2 = sum(1 for c in counts if c < 2)
        print(f"    {label:9} median={counts[len(counts)//2]:3} "
              f"p90={counts[int(0.9*(len(counts)-1))]:3} max={counts[-1]:3} "
              f"mean={sum(counts)/len(counts):5.2f}  <2 for {lt2/len(counts)*100:5.1f}% of wall"
              f"  (<1: {sum(1 for c in counts if c<1)/len(counts)*100:.1f}%)")

    end_of = lambda r: r["slot_fulfil"] or r["grain_done"] or r["filters_done"]
    # in flight = dispatched and not yet published (includes frames blocked on
    # a reference); running = past its reference waits, i.e. actually runnable.
    tick([(r["start"], end_of(r)) for r in fr.values() if r["start"]], "in-flight")
    tick([(r["refs_ready"], end_of(r)) for r in fr.values() if r["refs_ready"]], "running")
    tick([(r["hdr_done"], r["tile_done"]) for r in fr.values() if r["hdr_done"] and r["tile_done"]],
         "parsing")

    print("\n(e) dispatcher in-flight cap waits")
    for kind in ("decode", "show"):
        k = [(b, e) for t_, b, e in caps if t_ == kind]
        tot = sum(e - b for b, e in k)
        mx = max((e - b for b, e in k), default=0)
        print(f"    {kind:7} blocks={len(k):5} total={ms(tot):9.1f} ms "
              f"({tot/wall*100:5.1f}% of wall) max={ms(mx):.2f} ms")


if __name__ == "__main__":
    main()
