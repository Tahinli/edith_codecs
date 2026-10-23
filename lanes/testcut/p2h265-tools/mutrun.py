# usage: mutrun.py OUT NAMES_JSON TAG TIMEOUT_SECS FILTER_EXPR [--profile fast]
# Applies each named mutant's own diff (from OUT/mutants.json) to the lane tree, runs
# `cargo nextest run -p ec-h265 -E FILTER_EXPR`, restores the file, and records one row per mutant:
#   name <TAB> status <TAB> summary
# status: KILLED (test failure or timeout), SURVIVED (all pass), UNVIABLE (compile error), NORUN (no test ran).
# The tree is the lane's synced tree (cwd); only the one mutated file is touched, and it is restored.
import json, os, subprocess, sys, re
out, names_file, tag, timeout = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
expr = sys.argv[5]
profile = sys.argv[7] if len(sys.argv) > 7 and sys.argv[6] == "--profile" else None
names = json.load(open(names_file))
muts = {m["name"]: m for m in json.load(open(f"{out}/mutants.json"))}

def parse(diff):
    lines = diff.split("\n")
    fname = lines[0][4:].strip()
    hunks = []
    i = 1
    while i < len(lines):
        m = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", lines[i])
        if not m:
            i += 1
            continue
        start = int(m.group(1))
        body = []
        i += 1
        while i < len(lines) and not lines[i].startswith("@@"):
            if lines[i][:1] in (" ", "-", "+"):
                body.append((lines[i][:1], lines[i][1:]))
            i += 1
        hunks.append((start, body))
    return fname, hunks

def apply(orig, hunks):
    res, cur = [], 0
    for start, body in hunks:
        pos = start - 1
        res.extend(orig[cur:pos])
        cur = pos
        for op, text in body:
            if op in (" ", "-"):
                if cur >= len(orig) or orig[cur] != text:
                    raise ValueError(f"context mismatch at line {cur + 1}")
                if op == " ":
                    res.append(text)
                cur += 1
            else:
                res.append(text)
    res.extend(orig[cur:])
    return res

def sh(cmd, tmo):
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                         start_new_session=True)
    try:
        output, _ = p.communicate(timeout=tmo)
        return p.returncode, output, False
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, 9)
        output, _ = p.communicate()
        return -1, output or "", True

BUILD_TO = int(os.environ.get("BUILD_TO", "2400"))

def run_once():
    # release profile; build first (own generous timeout), then run with the measured-baseline timeout
    cmd = ["cargo", "nextest", "run", "--release", "-p", "ec-h265", "--no-fail-fast", "-E", expr]
    if os.environ.get("FAILFAST"):
        cmd.remove("--no-fail-fast")
    if profile:
        cmd[-2:-2] = ["--profile", profile]
    rc, out, to = sh([c for c in cmd if c != "--no-fail-fast"] + ["--no-run"], BUILD_TO)
    if to or rc != 0:
        return rc, out, "BUILDFAIL"
    return sh(cmd, timeout)

rows = []
for name in names:
    m = muts.get(name)
    if m is None:
        rows.append((name, "NORUN", "name not in mutants.json"))
        continue
    fname, hunks = parse(m["diff"])
    raw = open(fname, "rb").read()
    text = raw.decode()
    orig = text.split("\n")
    try:
        new = apply(orig, hunks)
    except ValueError as e:
        rows.append((name, "NORUN", f"apply failed: {e}"))
        continue
    try:
        open(fname, "w").write("\n".join(new))
        rc, output, timed_out = run_once()
        timed_out = timed_out
    finally:
        open(fname, "wb").write(raw)
    summary = re.findall(r"Summary \[[^\]]*\] (\d+)(?:/\d+)? tests? run", output)
    ran = int(summary[-1]) if summary else 0
    if timed_out == "BUILDFAIL":
        status = "UNVIABLE"
    elif timed_out:
        # A timeout is kill evidence only if the test run had started (build time alone is not).
        status = "KILLED" if re.search(r"Starting \d+ tests?", output) else "TIMEOUT_NOSTART"
    elif rc != 0 and re.search(r"EC_REQUIRE_FIXTURES|fixtures? (missing|not found)", output):
        # A fixture-missing failure is not a kill.
        status = "FIXTURE_FAIL"
    elif "could not compile" in output or "error[E" in output:
        status = "UNVIABLE"
    elif rc == 0 and ran > 0:
        status = "SURVIVED"
    elif rc != 0 and ran > 0:
        status = "KILLED"
    else:
        status = "NORUN"
    err = next((l for l in output.splitlines() if "error[" in l or l.startswith("error")), "") if status == "UNVIABLE" else ""
    rows.append((name, status, f"rc={rc} ran={ran} timeout={timed_out} {err}"))
    print(status, name[:90], f"rc={rc} ran={ran}", flush=True)

with open(f"{out}/res-{tag}.tsv", "w") as f:
    for r in rows:
        f.write("\t".join(r) + "\n")
print("summary", {s: sum(1 for r in rows if r[1] == s) for s in ("KILLED", "SURVIVED", "UNVIABLE", "NORUN")})
