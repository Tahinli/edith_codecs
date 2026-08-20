#!/usr/bin/env python3
"""Reads the delta every 3.0 dB envelope codeword carries off the reference
decoder's `env_facs_q V is invalid` range check, by ACCUMULATION.

`sbrtables.py --stage values` applies a codeword once to a raw start value and
reads the delta from the out-of-range complaint. The 3.0 dB books ride a 6-bit
(5-bit balance) raw field but are checked against a 7-bit ceiling, so a single
positive 3.0 dB delta never trips it and every one was emitted as 0
(ENV30_F/ENV30_T/ENVB30_F/ENVB30_T: 32/32/13/13 zeros).

The check sees the ACCUMULATED value, and delta-time coding accumulates across
envelopes and frames: a `_T` codeword repeated for enough envelopes walks any
non-zero delta out of range, and a `_F` codeword is walked out by trusted
negative `_T` steps riding after it. The complaint reports the first value out
of range (mod 256); a tiny simulator predicts that value for every candidate
delta and the one candidate that reproduces every probe is the answer. Trusted
negative entries go through the same readout and must come back unchanged.

Writes the values in place into crates/ec-aac/src/sbr_tables.rs.

    python3 scripts/aac-tables/sbrvalues.py
"""

import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import sbrtables as T  # noqa: E402
from sbrprobe import silent  # noqa: E402

RS = os.path.join(T.ROOT, "crates", "ec-aac", "src", "sbr_tables.rs")
KINDS = ["ENV30_T", "ENVB30_T", "ENV30_F", "ENVB30_F"]
INVALID = re.compile(r"env_facs_q (-?\d+) is invalid")
RUNS = 40


def load_books():
    src = open(RS).read()
    books = {}
    for m in re.finditer(r"static (\w+): \[\(u8, u32, i32\); \d+\] = \[(.*?)\n\];", src, re.S):
        books[m.group(1)] = {
            (int(a), int(b)): int(c)
            for a, b, c in re.findall(r"\((\d+), (\d+), (-?\d+)\)", m.group(2))
        }
    return books


def by_value(book, v):
    return T.as_bits(*next(lc for lc, d in book.items() if d == v))


def first_invalid(err):
    m = INVALID.search(err)
    return int(m.group(1)) if m else None


def simulate(steps, scale, raw):
    """First out-of-range `scale * value` (mod 256) along `raw + cumsum(steps)`."""
    acc = raw
    for s in steps:
        acc += s
        v = scale * acc
        if v < 0 or v > 127:
            return v % 256
    return None


def probe_t(kind, book, cfg, tables, word, raw, balance):
    """env0 F-coded from `raw` (no F slots: one band), env1 T-coded with `word`,
    then RUNS frames with both envelopes T-coded with `word`.  ch0 of a coupled
    pair rides trusted zero words."""
    key = "env0b" if balance else "env0"
    start = dict(cfg, **{key: [raw] * 8})
    run = dict(cfg, df_env=[1, 1] + [0] * 6, **{key: [raw] * 8})
    z = by_value(T_BOOK, 0)
    ch0 = [z] if balance else []
    frames = [silent(cfg["sf_index"]), T.frame(start, tables, ch0 + [word])]
    frames += [T.frame(run, tables, ch0 * 2 + [word, word])] * RUNS
    return b"".join(frames), [None] + [None] * (2 * RUNS)  # steps = word each


def probe_f(kind, book, cfg, tables, word, raw, balance, step_book, step):
    """env0 F-coded from `raw` with `word` in band 1, env1 T-coded with band 0
    zero and band 1 `step`, then RUNS frames of the same T pair."""
    key = "env0b" if balance else "env0"
    start = dict(cfg, df_env=[0, 1] + [0] * 6, **{key: [raw] * 8})
    run = dict(cfg, df_env=[1, 1] + [0] * 6, **{key: [raw] * 8})
    zf, zt = by_value(F_BOOK, 0), by_value(T_BOOK, 0)
    s = by_value(step_book, step)
    zs = by_value(T_BOOK if balance else step_book, 0)
    ch0 = [zf, zt, zt] if balance else []
    ch0r = [zt, zt, zt, zt] if balance else []
    frames = [silent(cfg["sf_index"]), T.frame(start, tables, ch0 + [word, zs, s])]
    frames += [T.frame(run, tables, ch0r + [zs, s, zs, s])] * 6
    return b"".join(frames), [None] + [step] * 13


def read_book(oracle, kind, books):
    book = books[kind]
    balance = kind.startswith("ENVB")
    scale = 2 if balance else 1
    span = 12 if balance else 31
    cands = range(-span, span + 1)
    cfg, tables = T.recipe_config(kind)
    raw_max = 31 if balance else 63
    out = {}
    for lc in sorted(book):
        word = T.as_bits(*lc)
        obs = []  # (steps template, raw, observed)
        alive = set(cands)
        # Probe until exactly one candidate reproduces every observation.
        if kind.endswith("_T"):
            plan = [("t", raw, None) for raw in range(raw_max, -1, -1)]
        else:
            sb = books["ENVB30_T" if balance else "ENV30_T"]
            neg = sorted({d for d in sb.values() if d < 0})
            plan = [("f", raw_max, s) for s in neg[:4]] + [("f", 0, s) for s in neg[:4]]
        for mode, raw, s in plan:
            if mode == "t":
                data, steps = probe_t(kind, book, cfg, tables, word, raw, balance)
            else:
                data, steps = probe_f(kind, book, cfg, tables, word, raw, balance, sb, s)
            err = oracle.one(f"acc:{kind}:{lc}:{mode}:{raw}:{s}", data)
            got = first_invalid(err)
            alive = {
                d for d in alive
                if simulate([d if x is None else x for x in steps], scale, raw) == got
            }
            if len(alive) <= 1:
                break
        if len(alive) != 1:
            print(f"  {kind} {lc}: UNRESOLVED, candidates {sorted(alive)}")
            continue
        d = next(iter(alive))
        old = book[lc]
        flag = "" if old == d else ("  (was 0)" if old == 0 else f"  <-- DISAGREES with {old}")
        print(f"  {kind} {lc}: {d:>4}{flag}")
        out[lc] = d
    vals = sorted(out.values())
    complete = vals == list(cands) and len(out) == len(book)
    print(f"{kind}: {len(out)}/{len(book)} read, values {'complete -%d..%d once each' % (span, span) if complete else 'NOT a permutation: ' + str(vals)}")
    return out, complete


def main():
    books = load_books()
    global F_BOOK, T_BOOK
    F_BOOK, T_BOOK = books["ENV30_F"], books["ENV30_T"]
    oracle = T.Oracle(12)
    out = {}
    try:
        for kind in KINDS:
            vals, complete = read_book(oracle, kind, books)
            if complete:
                out[kind] = vals
                books[kind] = vals  # later books ride on these
                if kind == "ENV30_T":
                    T_BOOK = vals
    finally:
        oracle.save()
    src = open(RS).read()

    def patch_block(m):
        name, body = m.group(1), m.group(2)
        if name not in out:
            return m.group(0)
        new = re.sub(
            r"\((\d+), (\d+), (-?\d+)\)",
            lambda t: f"({t.group(1)}, {t.group(2)}, {out[name][(int(t.group(1)), int(t.group(2)))]})",
            body,
        )
        return m.group(0).replace(body, new)

    src = re.sub(r"static (\w+): \[\(u8, u32, i32\); \d+\] = \[(.*?)\n\];", patch_block, src, flags=re.S)
    open(RS, "w").write(src)
    print(f"wrote {RS}: {sorted(out)}")


if __name__ == "__main__":
    main()
