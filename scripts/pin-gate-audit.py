#!/usr/bin/env python3
"""pin-gate-audit.py — census of the gates that read a PINNED fixture.

The r1 invariant was too weak: it matched only single-name call shapes
(`crate_pin("X")`, `pin_dir().join("X")`). It is blind to a much worse shape
that exists in the crate right now -- a gate that reads N pins through a
DIRECTORY literal plus a runtime-formatted name list:

    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
    ["warp-mismatch", "warp-flake-5", ...].iter().map(|n| format!("{fixtures}/{n}.obu"))

Fourteen names, none committed under the crate, so no clean tree and no runner
can satisfy them, and the gate is #[ignore]d -- a no-op that reports nothing.
A preflight that passes it is the silent shape again.

This audit, for every `#[test]` fn in crates/*/src and crates/*/tests that
reaches for a fixture:

  * resolves the fixture ROOT literal (concat! or Path::new(env!).join chain)
  * enumerates every runtime name built from that root
      - a `[..].iter().map(|n| format!("{var}/{n}.obu"))` name list
      - a `format!` naming the root variable directly
  * for each name, checks the path under the ROOT (gitignored, so never tracked)
    and under crates/<crate>/fixtures/ (the committed copy)
  * counts pin-reading gates, how many resolve through a committed copy, and how
    many do not -- printed as numbers, not prose

ASSERTION CLASSIFICATION uses the helper CALL GRAPH, not the gate body: a gate
that delegates to a helper (the warp gate calls check_pinned_warp_stream, which
asserts all three planes per frame) is NOT a no-assert gate, and reading only the
body gets that wrong -- which is exactly the mistake that produced the
"asserts nothing" classification earlier today.

Output (one record per line, tab separated fields):
    GATE   <file>:<line>\t<fn>\t<root-kind>\t<asserts:yes|no|via-helper NAME>\t<ignored:yes|no>\t<names>
    NAME   <file>:<line>\t<gate-fn>\t<name>\t<committed-copy:present|absent>\t<tracked:yes|no|->\t<exists-under-root:yes|no>
    COUNT  total=<n>\tcommitted=<n>\tuncommitted=<n>\tignored=<n>\tassertless=<n>

Exit 0 always: the caller decides what is a failure. Set PIN_AUDIT_STRICT=1 to
exit 1 when any name lacks a tracked committed copy.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def sources():
    for crate in sorted(os.listdir(os.path.join(ROOT, "crates"))):
        for sub in ("src", "tests"):
            d = os.path.join(ROOT, "crates", crate, sub)
            if not os.path.isdir(d):
                continue
            for name in sorted(os.listdir(d)):
                if name.endswith(".rs"):
                    yield crate, os.path.join(d, name)


def fn_bodies(text):
    """Yield (start_line, name, body, preceding_attrs) for every fn.

    The body runs from the `fn` line to the NEXT `fn` line rather than to a
    brace-balanced end: brace counting over 39k lines of Rust miscounts the
    moment one string literal or macro contains an unbalanced brace, and a
    single bad walk swallows every gate after it. Over-capturing trailing
    attributes of the next fn is harmless here -- attributes are read from the
    lines immediately above the `fn` line, which this method never crosses.
    """
    lines = text.splitlines()
    starts = []
    for i, line in enumerate(lines):
        m = re.match(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_0-9]+)", line)
        if m:
            starts.append((i, m.group(1)))
    for n, (i, name) in enumerate(starts):
        end = starts[n + 1][0] if n + 1 < len(starts) else len(lines)
        attrs = []
        j = i - 1
        while j >= 0 and lines[j].lstrip().startswith("#["):
            attrs.append(lines[j].strip())
            j -= 1
        yield i + 1, name, "\n".join(lines[i:end]), attrs


ROOT_RE = re.compile(
    r'concat!\(\s*env!\("CARGO_MANIFEST_DIR"\)\s*,\s*"([^"]*)"\s*\)'
)
JOIN_RE = re.compile(
    r'Path::(?:new|buf_from)\(\s*env!\("CARGO_MANIFEST_DIR"\)\s*\)\s*'
    r'(?:\.join\("([^"]*)"\)\s*)+'
)
LIST_RE = re.compile(r'\[\s*((?:[^\[\]"]*"[^"]*"\s*,?\s*)+)\]')
FMT_VAR_RE = re.compile(r'format!\(\s*"\{([a-z_0-9]+)\}/\{([a-z_0-9]+)\}(?:\.([a-z0-9]+))?"\s*\)')
FMT_LIT_RE = re.compile(r'format!\(\s*"\{([a-z_0-9]+)\}(/[A-Za-z0-9._-]+)"\s*\)')
HELPER_RE = re.compile(r'\b([a-z_0-9]+)\s*\(')


def tracked(path):
    if not os.path.exists(os.path.join(ROOT, ".git")):
        return "->"
    r = subprocess.run(
        ["git", "-C", ROOT, "ls-files", "--error-unmatch", "--", path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return "yes" if r.returncode == 0 else "no"


def main():
    helpers = {}          # fn name -> body (all fns in the tree)
    for crate, path in sources():
        text = open(path, encoding="utf-8", errors="replace").read()
        for _line, name, body, _attrs in fn_bodies(text):
            helpers.setdefault(name, body)

    total = committed = uncommitted = ignored = assertless = 0
    bad = []

    for crate, path in sources():
        text = open(path, encoding="utf-8", errors="replace").read()
        rel = os.path.relpath(path, ROOT)
        for line, name, body, attrs in fn_bodies(text):
            if not any(a.startswith("#[test") for a in attrs):
                continue
            root_lit = None
            root_kind = "none"
            var = None
            m = ROOT_RE.search(body)
            if m:
                root_lit, var = m.group(1), None
                root_kind = "concat!"
            else:
                m = JOIN_RE.search(body)
                if m:
                    joined = re.findall(r'\.join\("([^"]*)"\)', m.group(0))
                    root_lit = "".join(joined)
                    root_kind = "join"
            if root_lit is None:
                # The single-name shapes the FIRST invariant matched, kept so
                # the census counts every pin-reading gate, not only the ones
                # reached through a root literal.
                ign_s = "yes" if any("ignore" in a for a in attrs) else "no"
                singles = re.findall(r'crate_pin\(\s*"([^"]+)"', body)
                # A gate that builds the pin name at runtime (crate_pin(&format!(..)))
                # matches no single-name shape and would VANISH from the census --
                # the same blind spot in a new place. Surface it instead.
                for dyn_call in re.findall(r'crate_pin\(\s*&?\s*(?!")', body):
                    print("BADCALL\t{}\t{}\tcrate_pin argument is not a string literal: {}"
                          .format("{}:{}".format(rel, line), name, dyn_call.strip()[:60]))
                    bad.append("BADROW\t{}\t{}\tcrate_pin\tname built at runtime -- this census "
                               "cannot resolve it, so the gate is UNPROVEN here".format(
                                   "{}:{}".format(rel, line), name))
                singles += re.findall(
                    r'pin_dir\(\)\.join\(\s*"([^"]+)"', body)
                if not singles:
                    continue
                total += 1
                if ign_s == "yes":
                    ignored += 1
                for n in singles:
                    committed_rel = "crates/{}/fixtures/{}".format(crate, n)
                    present = os.path.isfile(os.path.join(ROOT, committed_rel))
                    tr = tracked(committed_rel) if present else "no"
                    total_names = 1
                    print("GATE\t{}\t{}\tcrate_pin\tyes\t{}\t{}".format(
                        "{}:{}".format(rel, line), name, ign_s, len(singles)))
                    print("NAME\t{}\t{}\t{}\t{}\t{}\t{}".format(
                        "{}:{}".format(rel, line), name, n,
                        "present" if present else "absent", tr, "yes"))
                    if present and tr in ("yes", "->"):
                        committed += 1
                    else:
                        uncommitted += 1
                        bad.append("BADROW\t{}\t{}\tcrate_pin\t{} not committed+tracked".format(
                            "{}:{}".format(rel, line), name, n))
                continue

            # the variable the root is bound to, if any
            v = re.search(
                r'let\s+([a-z_0-9]+)\s*=\s*(?:concat!\(\s*env!\("CARGO_MANIFEST_DIR"\)|'
                r'Path::(?:new|buf_from)\(\s*env!\("CARGO_MANIFEST_DIR"\)\s*\)\s*(?:\.join\("[^"]*"\)\s*)+)',
                body,
            )
            if v:
                var = v.group(1)

            # names: ONLY the two shapes that matter.
            #   (i) a runtime NAME LIST formatted against the root variable --
            #       the shape the weak invariant was blind to
            #   (ii) a single file literal appended to the root
            # Anything else (a format! over some other variable, a scratch dump
            # path) is not a pin read and is left to the manifest scan.
            names = []
            if var:
                # The name list is bracketed, but comments sit between its
                # items ("lane-rect r2: HORP strip ..."), so strip line
                # comments before parsing the list and anchor on the format!
                # that consumes it.
                body_nc = re.sub(r"//[^\n]*", "", body)
                for lit in LIST_RE.finditer(body_nc):
                    items = re.findall(r'"([A-Za-z0-9._-]+)"', lit.group(1))
                    if len(items) < 3:
                        continue
                    fm = FMT_VAR_RE.search(body_nc[lit.end():lit.end() + 400])
                    if not fm or fm.group(1) != var:
                        continue
                    ext = fm.group(3) or ""
                    for n in items:
                        names.append((n, ext))
            if not names:
                lit = re.search(r'"(/[A-Za-z0-9._/-]+\.[A-Za-z0-9]+)"', body)
                # a lane dump under lanes/ is not a fixture and is not this gate
                if lit and "/fixtures/" in lit.group(1):
                    names.append((os.path.basename(lit.group(1)), ""))
            if not names:
                continue

            # assertion classification through the CALL GRAPH
            asserts = "assert" in body
            via = ""
            if not asserts:
                for h in HELPER_RE.findall(body):
                    hb = helpers.get(h)
                    if hb and "assert" in hb:
                        asserts = True
                        via = h
                        break
            ign = "yes" if any("ignore" in a for a in attrs) else "no"
            if not names:
                continue
            total += 1
            if ign == "yes":
                ignored += 1
            if not asserts:
                assertless += 1
            kind = "yes" if asserts else "no"
            if asserts and via:
                kind = "via-" + via
            print("GATE\t{}\t{}\t{}\t{}\t{}\t{}".format(
                "{}:{}".format(rel, line), name, root_kind, kind, ign, len(names)))
            missing = []
            missing = []
            missing = []
            for n, _ext in names:
                committed_rel = "crates/{}/fixtures/{}".format(crate, n)
                present = os.path.isfile(os.path.join(ROOT, committed_rel))
                tr = tracked(committed_rel) if present else "no"
                under_root = os.path.isfile(os.path.join(ROOT, root_lit.strip("/"), n)) \
                    if root_lit.startswith("..") else os.path.isfile(
                        os.path.join(ROOT, "fixtures", n))
                print("NAME\t{}\t{}\t{}\t{}\t{}\t{}".format(
                    "{}:{}".format(rel, line), name, n,
                    "present" if present else "absent", tr,
                    "yes" if under_root else "no"))
                if present and tr in ("yes", "->"):
                    committed += 1
                else:
                    uncommitted += 1
                    missing.append(n)
            if missing:
                bad.append("BADROW\t{}\t{}\t{}\t{}/{} pin(s) not committed+tracked, e.g. {}".format(
                    "{}:{}".format(rel, line), name, root_kind, len(missing), len(names), missing[0]))
            # a bare directory root feeding a runtime name list can never be
            # satisfied by a committed tree
            if names and root_lit.rstrip("/").endswith("fixtures"):
                print("BARDIR\t{}\t{}\t{} + {} runtime name(s)".format(
                    "{}:{}".format(rel, line), name, root_lit, len(names)))
                bad.append("BADROW\t{}\t{}\t{}\tbare directory literal feeding {} runtime name(s): no committed tree satisfies it".format(
                    "{}:{}".format(rel, line), name, root_lit, len(names)))

    print("COUNT\ttotal={}\tcommitted={}\tuncommitted={}\tignored={}\tassertless={}".format(
        total, committed, uncommitted, ignored, assertless))
    if bad:
        print("BAD\t{}".format(len(bad)))
        for b in sorted(set(bad)):
            print(b)
    if os.environ.get("PIN_AUDIT_STRICT") == "1" and bad:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
