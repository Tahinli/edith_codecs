#!/usr/bin/env python3
"""Phase 2 evidence driver for ec-h264 (runs on a server inside the lane tree).

Stages (argv[1]):
  cov   coverage of fast suite and each candidate; prints U(T) sizes
  run   full pipeline: coverage, sampling, S/K/K' kill runs, decisions

Output that matters goes to stdout as lines starting with 'RESULT\t'.
Scratch (cargo-mutants trees) goes under $TMPDIR, which is not synced.
"""
import json, os, re, shutil, subprocess, sys, glob

CRATE = "ec-h264"
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
TMP = os.environ.get("TMPDIR", "/tmp")
WORK = os.path.join(TMP, "p2h264")
N_SAMPLE = int(os.environ.get("N_SAMPLE", "30"))
ENV = dict(os.environ, EC_REQUIRE_FIXTURES="1")
TARGET = os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target"))

# (test, runtime s, class). Order = decreasing runtime, as the protocol requires.
CANDS = [
    ("every_quantiser_with_motion_matches_ffmpeg", 70.90, "candidate"),
    ("every_quantiser_matches_ffmpeg", 70.34, "candidate"),
    ("high_profile_8x8_matches_ffmpeg", 56.82, "candidate"),
    ("x264_hi444_10bit", 24.18, "candidate"),
    ("corrupt_streams_never_panic", 24.24, "pseudo"),
    ("x264_hi444_8bit", 23.01, "candidate"),
    ("truncated_and_corrupted_conformance_streams_never_panic", 20.88, "pseudo"),
]
PROTECTED = {"jvt_hi444_vectors_bit_exact": 464.6}
EXCLUDE_ALWAYS = "encode::real_library_frames_encode_and_decode_exactly"


def sel(name):
    return "test(/(^|::)%s$/)" % name


def sh(args, log):
    os.makedirs(WORK, exist_ok=True)
    with open(os.path.join(WORK, log), "w") as f:
        r = subprocess.run(args, cwd=ROOT, env=ENV, stdout=f, stderr=subprocess.STDOUT)
    print("STEP %s rc=%d" % (log, r.returncode), flush=True)
    return r.returncode


def clear_profraw():
    for p in glob.glob(os.path.join(TARGET, "llvm-cov-target", "*.profraw")):
        os.remove(p)


VALID = {}
FAST_CACHE = {}
LAST_BAD = 0
LAST_RC = 0


def prune_bad_profraw(files):
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
    prof = os.path.join(sysroot, "lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata")
    n = 0
    for f in files:
        r = subprocess.run([prof, "show", f], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if r.returncode != 0:
            os.remove(f)
            n += 1
    print("PRUNED bad profraw: %d" % n, flush=True)
    return n


def tail_log(log, n=40):
    p = os.path.join(WORK, log)
    if os.path.exists(p):
        with open(p, errors="replace") as f:
            lines = f.readlines()[-n:]
        print("LOG-TAIL %s\n%s" % (log, "".join(lines)), flush=True)


def cov_report(out_json, extra_args):
    """Run instrumented tests (profraw kept fresh), then dump JSON.

    --no-fail-fast: wall-clock tripwires can fail under instrumentation; the
    profraw is still written, so a nonzero run exit is only reported.
    """
    subprocess.run(["cargo", "llvm-cov", "clean", "--profraw-only"], cwd=ROOT, env=ENV,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    clear_profraw()
    base = os.path.basename(out_json)
    rc_run = sh(["cargo", "llvm-cov", "nextest", "--release", "-p", CRATE, "--no-report",
                 "--no-fail-fast"] + extra_args, base + ".run.log")
    files = sorted(glob.glob(os.path.join(TARGET, "llvm-cov-target", "*.profraw")))
    global LAST_BAD
    LAST_BAD = prune_bad_profraw(files)
    files = sorted(glob.glob(os.path.join(TARGET, "llvm-cov-target", "*.profraw")))
    print("PROFRAW %s n=%d sizes=%s" % (base, len(files),
          [os.path.getsize(f) for f in files][:20]), flush=True)
    rc_rep = sh(["cargo", "llvm-cov", "report", "--release", "--json", "--output-path", out_json],
                base + ".rep.log")
    if rc_rep != 0 or not os.path.exists(out_json):
        print("FAIL coverage report for %s (run rc=%d, report rc=%d)" % (base, rc_run, rc_rep),
              flush=True)
        tail_log(base + ".run.log")
        tail_log(base + ".rep.log")
        sys.exit(3)
    global LAST_RC
    LAST_RC = rc_run
    if rc_run != 0:
        print("NOTE coverage run for %s exited %d (test failures, profraw kept)" % (base, rc_run),
              flush=True)
    with open(out_json) as f:
        return json.load(f)


def regions(report):
    """key (file, l1, c1, l2, c2) -> (max count, function name), code regions only."""
    out = {}
    for fn in report["data"][0]["functions"]:
        for r in fn["regions"]:
            l1, c1, l2, c2, cnt, fid, _exp, kind = r[:8]
            if kind != 0:
                continue
            fname = fn["filenames"][fid]
            if "/crates/%s/src/" % CRATE not in fname:
                continue
            rel = fname[fname.index("crates/"):]
            key = (rel, l1, c1, l2, c2)
            prev = out.get(key)
            if prev is None or cnt > prev[0]:
                out[key] = (cnt, fn["name"])
    return out


def innermost(regs, rel, line):
    best = None
    for key, (cnt, _f) in regs.items():
        if key[0] != rel or not (key[1] <= line <= key[3]):
            continue
        span = (key[3] - key[1], key[4] - key[2] if key[1] == key[3] else 0)
        if best is None or span < best[0]:
            best = (span, key, cnt)
    return best


def mutants_list():
    txt = subprocess.run(["cargo", "mutants", "-p", CRATE, "--list"], cwd=ROOT, env=ENV,
                         capture_output=True, text=True).stdout
    out = []
    for line in txt.splitlines():
        m = re.match(r"^(crates/%s/src/[^:]+):(\d+):(\d+): " % CRATE, line)
        if m:
            out.append((line.strip(), m.group(1), int(m.group(2))))
    return out


def mutate(outdir, names, extra_test_args, timeout_is_kill):
    """Return (caught, not_killed) name sets for the given mutants.

    The unmutated baseline runs in the scratch copy (default baseline). If it
    fails, cargo-mutants exits 4 and we stop without deciding anything.
    `--gitignore false` copies the ignored `fixtures` symlink into the scratch tree.
    Timeouts count as kills only when timeout_is_kill (the candidate's own run).
    """
    if not names:
        return set(), set()
    if os.path.isdir(outdir):
        assert outdir.startswith(WORK + os.sep), outdir
        shutil.rmtree(outdir)
    os.makedirs(outdir, exist_ok=True)
    tag = os.path.basename(outdir)
    args = ["cargo", "mutants", "-p", CRATE, "--test-tool", "nextest",
            "--timeout-multiplier", "3", "-j", "1", "--copy-vcs", "true",
            "--gitignore", "false", "-o", outdir]
    for n in names:
        args += ["--re", re.escape(n)]
    # Needs the user's personal media, which no server has: excluded from every mutation run.
    excl = "not test(=%s)" % EXCLUDE_ALWAYS
    extra = list(extra_test_args)
    if "-E" in extra:
        i = extra.index("-E") + 1
        extra[i] = "(%s) and %s" % (extra[i], excl)
    else:
        extra += ["-E", excl]
    for a in extra:
        args += ["--cargo-test-arg=" + a]
    rc = sh(args, tag + ".log")
    if rc == 4:
        print("RESULT\tbaseline_failed\t%s" % tag, flush=True)
        sys.exit(4)
    base = glob.glob(os.path.join(outdir, "**", "mutants.out"), recursive=True)
    if not base:
        print("RESULT\tmutate_no_output\t%s\trc=%d" % (tag, rc), flush=True)
        sys.exit(5)
    d = base[0]

    def read(name):
        p = os.path.join(d, name)
        return set(l.strip() for l in open(p)) if os.path.exists(p) else set()

    caught = read("caught.txt")
    timeouts = read("timeout.txt")
    missed = read("missed.txt")
    if timeout_is_kill:
        caught |= timeouts
    else:
        missed |= timeouts
    print("MUTSTAT %s caught=%d missed=%d timeout=%d unviable=%d asked=%d rc=%d" % (
        tag, len(caught), len(missed), len(timeouts), len(read("unviable.txt")), len(names), rc), flush=True)
    # Sanity: fixture/runner errors must not be what kills a mutant.
    logs = glob.glob(os.path.join(d, "log", "*.log"))
    bad = [p for p in logs if "lists no" in open(p, errors="replace").read()]
    print("SANITY %s logs=%d fixture_errors=%d" % (tag, len(logs), len(bad)), flush=True)
    for p in logs:
        hits = [l.rstrip() for l in open(p, errors="replace") if "panicked at" in l]
        if hits:
            print("SANITY %s sample_log=%s" % (tag, p), flush=True)
            for h in hits[:3]:
                print("SANITY   " + h[:200], flush=True)
            break
    return caught, missed


def run_cov(cands_to_check):
    fast = cov_report(os.path.join(WORK, "fast.json"),
                      ["--profile", "fast"])
    fast_r = regions(fast)
    per = {}
    for name, _s, _c in cands_to_check:
        rep = cov_report(os.path.join(WORK, name + ".json"), ["-E", sel(name)])
        tr = regions(rep)
        VALID[name] = (LAST_BAD == 0 and LAST_RC == 0)
        print("RESULT\tvalid\t%s\t%s\tbad_profraw=%d\trc=%d" % (name, VALID[name], LAST_BAD, LAST_RC), flush=True)
        U = [k for k, (c, _f) in tr.items() if c > 0 and fast_r.get(k, (0,))[0] == 0]
        per[name] = (tr, U)
        funcs = sorted({tr[k][1] for k in U})
        print("RESULT\tcov\t%s\tU=%d\tfuncs=%s" % (name, len(U), ";".join(funcs)[:400]), flush=True)
    return fast_r, per


def main():
    stage = sys.argv[1] if len(sys.argv) > 1 else "run"
    os.makedirs(WORK, exist_ok=True)
    if stage == "cov":
        run_cov(CANDS)
        return
    # full pipeline; start from a clean instrumented tree (as the ec-aac lane does)
    subprocess.run(["cargo", "llvm-cov", "clean", "--workspace"], cwd=ROOT, env=ENV,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    fast_r, per = run_cov(CANDS)
    muts = mutants_list()
    print("RESULT\tmutants_listed\t%d" % len(muts), flush=True)
    deleted = set()
    heavy_all = [n for n, _s, _c in CANDS] + list(PROTECTED)
    for name, secs, cls in CANDS:
        remaining = [h for h in heavy_all if h not in deleted and h != name]
        tr, U = per[name]
        if not VALID.get(name):
            print("RESULT\tdecision\t%s\t%s\tsecs=%.1f\tU=%d\ttried=0\tS=0\tK=0\tKp=0\tkeep (no valid evidence)\tex=" % (
                CRATE, name, secs, len(U)), flush=True)
            continue
        # mutants inside code T executes
        eligible = []
        for line_txt, rel, line in muts:
            hit = innermost(tr, rel, line)
            if hit is None or hit[2] == 0:
                continue
            fh = innermost(fast_r, rel, line)
            fast_hit = fh[2] if fh else 0
            in_u = hit[1] in U
            eligible.append((0 if in_u else 1, fast_hit, line_txt))
        eligible.sort()
        sample = [e[2] for e in eligible[:N_SAMPLE]]
        tag = name[:24]
        # S: survivors of the fast suite
        new = [m for m in sample if m not in FAST_CACHE]
        fc, fm = mutate(os.path.join(WORK, "fast-" + tag), new, ["--profile", "fast"], False)
        print("SANITY\t%s\tmissed=%d\tcaught=%d" % (name, len(fm), len(fc)), flush=True)
        for m in fc:
            FAST_CACHE[m] = True
        for m in fm:
            FAST_CACHE[m] = False
        viable = [m for m in sample if m in FAST_CACHE]
        if not viable:
            print("RESULT\tdecision\t%s\t%s\tsecs=%.1f\tU=%d\ttried=%d\tS=0\tK=0\tKp=0\tkeep (no valid evidence: 0 viable mutants)\tex=" % (
                CRATE, name, secs, len(U), len(sample)), flush=True)
            continue
        S = sorted(m for m in viable if not FAST_CACHE[m])
        # K: survivors of fast that T kills
        tc, tm = mutate(os.path.join(WORK, "t-" + tag), S,
                        ["-E", sel(name)], True)
        K = sorted(set(S) - tm)
        # K': K mutants that no other remaining heavy test kills
        if K and remaining:
            expr = " | ".join(sel(h) for h in remaining)
            kc, km = mutate(os.path.join(WORK, "k-" + tag), K, ["-E", expr], False)
            Kp = sorted(set(K) - kc)
        else:
            Kp = list(K)
        if cls == "pseudo":
            decision = "delete" if not K else "keep (weak oracle, kills %d)" % len(K)
        else:
            decision = "delete" if not Kp else "keep"
        if decision == "delete" and not PROTECTED.get(name):
            deleted.add(name)
        tried = len(viable)
        ex = Kp[0] if Kp else (K[0] if K and cls == "pseudo" else "")
        print("RESULT\tdecision\t%s\t%s\tsecs=%.1f\tU=%d\ttried=%d\tS=%d\tK=%d\tKp=%d\t%s\tex=%s" % (
            CRATE, name, secs, len(U), tried, len(S), len(K), len(Kp), decision,
            ex), flush=True)
    print("RESULT\tdeleted\t%s" % ",".join(sorted(deleted)), flush=True)


if __name__ == "__main__":
    main()
