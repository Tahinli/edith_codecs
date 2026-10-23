#!/usr/bin/env python3
"""Local analysis for the batched kill matrix (see lanes/testcut.phase2.md, "Batched variant").

Subcommands (paths are absolute or relative to the lane root):

  sample    MUTANTS.tsv COVDIR N SEED OUT_IDX OUT_JSON
              Stratified per-file seeded sample of mutants whose start line is covered by at
              least one heavy test. OUT_JSON holds the sample (indices 0..N-1 in OUT_IDX).
  survivors MUTANTS.json FASTDIR OUT_IDX
              Indices whose fast-suite status is "pass" (survived the fast suite).
  heavyjobs MUTANTS.json COVDIR SURV_IDX OUT_IDXFILE TESTSDIR
              For each survivor, the heavy tests whose coverage includes its line. Writes
              "<idx>\\t<testsfile>" lines for scripts/kill-matrix.sh mutrun heavy.
  decide    MUTANTS.json COVDIR FASTCOV.json FASTDIR HEAVYDIR TIMES.tsv KILLTSV REPORT_MD
  failing   LOG TESTS OUT
              Test names from TESTS that nextest marks FAIL/TIMEOUT/signal in LOG (heavy mode).
  compact   LLVM_COV.json OUT.json
              Reduce one llvm-cov JSON to executed ec-av1 regions (run on the server to keep
              the copied-back data small).

COVDIR holds index.tsv (ordinal, test name) and <ordinal>.json per heavy test.
"""
import json
import os
import random
import re
import sys
from collections import defaultdict

PROTECTED = re.compile(r"pinned|pins?_|_pins?\b|golden|sha256|vector|conformance|fuzz|realtime|faster|speedup|beats_the_incumbent")


def compact(cov_json_path, out_path):
    """Reduce an `llvm-cov report --json` file to the ec-av1 regions executed at least once.

    Output: JSON list of [file, line_start, col_start, line_end, col_end].
    """
    cov = json.load(open(cov_json_path))
    keys = set()
    for fn in cov["data"][0]["functions"]:
        names = fn.get("filenames", [])
        for r in fn["regions"]:
            if r[4] <= 0:
                continue
            f = names[r[5]]
            if "crates/ec-av1/src/" in f:
                keys.add((f, r[0], r[1], r[2], r[3]))
    json.dump(sorted(list(k) for k in keys), open(out_path, "w"))
    return keys


def load_regions(path):
    return {tuple(k) for k in json.load(open(path))}


def norm(f):
    """Workspace-relative path from "crates/" on (LLVM paths are absolute, mutant paths relative)."""
    return f[f.index("crates/"):] if "crates/" in f else f


def covered_lines(regions):
    lines = defaultdict(set)  # normalized file -> set of lines covered
    for (f, l1, _, l2, _) in regions:
        lines[norm(f)].update(range(l1, l2 + 1))
    return lines


def load_index(covdir):
    rows = []
    with open(os.path.join(covdir, "index.tsv")) as fh:
        for line in fh:
            i, name = line.rstrip("\n").split("\t")
            rows.append((i, name))
    return rows


def covers_line(cov_lines, mutant):
    line = mutant["span"]["start"]["line"]
    return line in cov_lines.get(norm(mutant["file"]), set())


def read_mutants_tsv(path):
    """All mutants, in `cargo mutants --list --json` order: one TSV row per mutant (see README in the report)."""
    rows = []
    for line in open(path):
        f, sl, sc, el, ec, rep, fn, genre, name = line.rstrip("\n").split("\t", 8)
        rows.append({"file": f, "span": {"start": {"line": int(sl), "column": int(sc)},
                                         "end": {"line": int(el), "column": int(ec)}},
                     "replacement": rep, "function": fn, "genre": genre, "name": name})
    return rows


def cmd_sample(tsv_path, covdir, n, seed, out_idx, out_json):
    """Seeded, per-file stratified sample of mutants whose start line is covered by a heavy test.

    out_json is the sample itself as a JSON array (same fields mutate.py reads). Its position is
    the sample index used by the other subcommands; "global" keeps the index in the full list.
    """
    mutants = read_mutants_tsv(tsv_path)
    union = defaultdict(set)
    for i, _ in load_index(covdir):
        for f, ls in covered_lines(load_regions(os.path.join(covdir, f"{i}.json"))).items():
            union[f] |= ls
    by_file = defaultdict(list)
    for gidx, m in enumerate(mutants):
        if covers_line(union, m):
            by_file[m["file"]].append(gidx)
    total = sum(len(v) for v in by_file.values())
    rng = random.Random(seed)
    picked = []
    for f in sorted(by_file):
        quota = max(1, round(n * len(by_file[f]) / total))
        picked += rng.sample(by_file[f], min(quota, len(by_file[f])))
    picked = sorted(rng.sample(picked, min(n, len(picked))))
    sample = []
    for g in picked:
        m = dict(mutants[g])
        m["global"] = g
        sample.append(m)
    json.dump(sample, open(out_json, "w"))
    with open(out_idx, "w") as fh:
        fh.write("".join(f"{i}\n" for i in range(len(sample))))
    print(f"eligible={total} files={len(by_file)} sampled={len(sample)} seed={seed}")


def read_statuses(d):
    st = {}
    for name in os.listdir(d):
        if name.endswith(".status"):
            st[int(name.split(".")[0])] = open(os.path.join(d, name)).read().strip()
    return st


def cmd_survivors(mutants_path, fastdir, out):
    st = read_statuses(fastdir)
    surv = sorted(i for i, s in st.items() if s == "pass")
    open(out, "w").write("".join(f"{i}\n" for i in surv))
    print(f"fast_run={len(st)} survivors={len(surv)} killed_by_fast={sum(1 for s in st.values() if s == 'fail')} unviable={sum(1 for s in st.values() if s == 'unviable')}")


def cmd_heavyjobs(mutants_path, covdir, surv_path, out, testsdir):
    mutants = json.load(open(mutants_path))
    names = dict(load_index(covdir))
    cov = {}
    for i in names:
        cov[i] = covered_lines(load_regions(os.path.join(covdir, f"{i}.json")))
    os.makedirs(testsdir, exist_ok=True)
    with open(out, "w") as fh:
        for line in open(surv_path):
            idx = int(line)
            m = mutants[idx]
            tests = [names[i] for i in names if covers_line(cov[i], m)]
            tfile = os.path.join(testsdir, f"{idx}.txt")
            with open(tfile, "w") as tf:
                tf.write("".join(t + "\n" for t in tests))
            fh.write(f"{idx}\t{tfile}\n")
            print(f"survivor {idx}: {m['file']}:{m['span']['start']['line']} covering_heavy={len(tests)}")


def cmd_decide(mutants_path, covdir, fastcov_path, fastdir, heavydir, times_path, killtsv, report_md):
    mutants = json.load(open(mutants_path))
    names = dict(load_index(covdir))
    regs = {}
    keep_names = set()
    for fn in ("failed.txt", "nocov.txt"):
        path = os.path.join(covdir, fn)
        if os.path.exists(path):
            keep_names |= {l.strip() for l in open(path) if l.strip()}
    for i, name in names.items():
        jpath = os.path.join(covdir, f"{i}.json")
        if os.path.exists(jpath):
            regs[name] = load_regions(jpath)
        else:
            keep_names.add(name)
            regs[name] = set()
    fast = load_regions(fastcov_path)
    secs = {}
    for line in open(times_path):
        parts = line.rstrip("\n").split("\t")
        if len(parts) == 2:
            secs[parts[0]] = float(parts[1])
    # Kill matrix: survivor -> failing heavy tests
    kills = {}
    for name in os.listdir(heavydir):
        if name.endswith(".fail"):
            idx = int(name.split(".")[0])
            kills[idx] = set(l.strip() for l in open(os.path.join(heavydir, name)) if l.strip())
    for idx in list(kills):
        if not kills[idx]:
            del kills[idx]
    # Greedy runtime-weighted set cover over the kill matrix (Smith & Kapfhammer).
    universe = set(kills)
    kept, uncovered = [], set(universe)
    tests_of = defaultdict(set)
    for idx, ts in kills.items():
        for t in ts:
            tests_of[t].add(idx)
    while uncovered:
        best = max(tests_of, key=lambda t: (len(tests_of[t] & uncovered) / secs.get(t, 1.0), t), default=None)
        if best is None or not (tests_of[best] & uncovered):
            break
        kept.append(best)
        uncovered -= tests_of[best]
    kept_set = set(kept)
    kept_regions = set(fast)
    for t in kept:
        kept_regions |= set(regs[t])
    rows = []
    deleted_secs = 0.0
    with open(killtsv, "w") as kt:
        kt.write("mutant\tfile\tline\treplacement\tkilling_heavy_tests\n")
        for idx in sorted(kills):
            m = mutants[idx]
            kt.write(f"{idx}\t{m['file']}\t{m['span']['start']['line']}\t{m['replacement']}\t{' ; '.join(sorted(kills[idx]))}\n")
    for name in sorted(names.values(), key=lambda n: -secs.get(n, 0)):
        u_fast = set(regs[name]) - set(fast)
        u_kept = set(regs[name]) - kept_regions
        kp = {idx for idx, ts in kills.items() if name in ts and not (ts & kept_set)}
        if name in kept_set:
            cls, action = "kept(cover)", "keep"
        elif name in keep_names:
            cls, action = "candidate", "keep (failed or no coverage)"
        elif PROTECTED.search(name):
            cls, action = "protected", "keep (protected)"
        else:
            cls = "candidate"
            if u_kept:
                action = "keep (unique coverage)"
            elif kp:
                action = "keep (unique kills)"
            else:
                action = "delete"
                deleted_secs += secs.get(name, 0)
        rows.append((name, cls, len(u_fast), len(u_kept), len(kp), action))
    with open(report_md, "w") as rp:
        rp.write("| crate | test | seconds | class | |U(T)| vs fast | |U(T)| vs fast+kept | |K'| | decision |\n")
        rp.write("|---|---|---|---|---|---|---|---|\n")
        for name, cls, uf, uk, kp_n, action in rows:
            rp.write(f"| ec-av1 | {name} | {secs.get(name, 0):.1f} | {cls} | {uf} | {uk} | {kp_n} | {action} |\n")
    deletions = [r[0] for r in rows if r[5] == "delete"]
    print(f"survivors_with_kills={len(kills)} kept_by_cover={len(kept)} candidates={sum(1 for r in rows if r[1]=='candidate')} deletable={len(deletions)} seconds_saved={deleted_secs:.1f}")
    with open(os.path.join(os.path.dirname(report_md), os.path.basename(report_md) + ".deletions"), "w") as fh:
        fh.write("".join(d + "\n" for d in deletions))


def cmd_failing(log_path, tests_path, out_path):
    """Names from TESTS that nextest reports as FAIL/TIMEOUT/signal in LOG (one per line in OUT)."""
    names = [l.strip() for l in open(tests_path) if l.strip()]
    status = re.compile(r"^\s*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGILL|SIGBUS|SIGFPE|SIGTERM|LEAK|ABORT)\b")
    text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", open(log_path, errors="replace").read())
    bad = set()
    for line in text.splitlines():
        if status.match(line):
            tail = line.rstrip()
            bad.update(n for n in names if tail.endswith(" " + n))
    open(out_path, "w").write("".join(f"{n}\n" for n in sorted(bad)))
    print(f"failing={len(bad)}")


def cmd_regex(sample_json, idx):
    """Anchored, escaped regex that selects exactly one mutant by its `--list` name."""
    name = json.load(open(sample_json))[int(idx)]["name"]
    special = set(".+*?()|[]{}^$\\#&-~")
    print("^" + "".join("\\" + c if c in special else c for c in name) + "$")


def cmd_name(sample_json, idx):
    print(json.load(open(sample_json))[int(idx)]["name"])


if __name__ == "__main__":
    cmd = sys.argv[1]
    args = sys.argv[2:]
    if cmd == "regex":
        cmd_regex(args[0], args[1])
    elif cmd == "name":
        cmd_name(args[0], args[1])
    elif cmd == "compact":
        compact(args[0], args[1])
    elif cmd == "sample":
        cmd_sample(args[0], args[1], int(args[2]), int(args[3]), args[4], args[5])
    elif cmd == "survivors":
        cmd_survivors(args[0], args[1], args[2])
    elif cmd == "heavyjobs":
        cmd_heavyjobs(args[0], args[1], args[2], args[3], args[4])
    elif cmd == "decide":
        cmd_decide(*args)
    elif cmd == "failing":
        cmd_failing(args[0], args[1], args[2])
    else:
        sys.exit(__doc__)
