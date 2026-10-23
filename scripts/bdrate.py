#!/usr/bin/env python3
"""Bjontegaard delta rate and delta PSNR between two rate-distortion curves.

Usage: python3 -I bdrate.py [--metric y|yuv] <anchor.tsv> <test.tsv>

Each TSV holds one operating point per line: `bytes<TAB>psnr_y<TAB>psnr_yuv`.
`--metric` picks the PSNR column the curves are judged by (default yuv). Four
points per curve is the usual Bjontegaard setup. The curves are fitted as cubic polynomials of
log10(bytes) against PSNR (least squares with exact interpolation at four
points), integrated over the PSNR range both cover.

Prints `bd_rate_pct` (negative: the test curve needs fewer bytes for the same
quality) and `bd_psnr_db` (positive: the test curve has more quality for the
same bytes).
"""
import math
import sys


def load(path, metric):
    col = {"y": 1, "yuv": 2}[metric]
    pts = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            pts.append((float(fields[0]), float(fields[col])))
    return pts


def solve(a, b):
    """Solve a x = b by Gaussian elimination with partial pivoting."""
    n = len(b)
    m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(m[r][col]))
        m[col], m[piv] = m[piv], m[col]
        for r in range(n):
            if r != col and m[col][col] != 0:
                f = m[r][col] / m[col][col]
                for c in range(col, n + 1):
                    m[r][c] -= f * m[col][c]
    return [m[i][n] / m[i][i] for i in range(n)]


def fit(xs, ys):
    """Cubic a0 + a1 x + a2 x^2 + a3 x^3 through the points (least squares
    via the normal equations; exact for four points)."""
    rows = [[x ** k for k in range(4)] for x in xs]
    ata = [[sum(r[i] * r[j] for r in rows) for j in range(4)] for i in range(4)]
    aty = [sum(r[i] * y for r, y in zip(rows, ys)) for i in range(4)]
    return solve(ata, aty)


def integrate(coef, lo, hi):
    def anti(x):
        return sum(coef[k] * x ** (k + 1) / (k + 1) for k in range(4))
    return (anti(hi) - anti(lo)) / (hi - lo)


def main() -> int:
    args = sys.argv[1:]
    metric = "yuv"
    if args[:1] == ["--metric"]:
        metric = args[1]
        args = args[2:]
    anchor = load(args[0], metric)
    test = load(args[1], metric)
    a_psnr = [p for _, p in anchor]
    t_psnr = [p for _, p in test]
    lo = max(min(a_psnr), min(t_psnr))
    hi = min(max(a_psnr), max(t_psnr))
    if hi <= lo:
        print("bd_rate_pct=nan bd_psnr_db=nan (PSNR ranges do not overlap)")
        return 1
    a_log = [math.log10(s) for s, _ in anchor]
    t_log = [math.log10(s) for s, _ in test]
    ca = fit(a_psnr, a_log)
    ct = fit(t_psnr, t_log)
    avg_a = integrate(ca, lo, hi)
    avg_t = integrate(ct, lo, hi)
    bd_rate = (10 ** (avg_t - avg_a) - 1) * 100

    # BD-PSNR: fit PSNR against log rate and average over the rate overlap.
    la = [math.log10(s) for s, _ in anchor]
    lt = [math.log10(s) for s, _ in test]
    lo_r = max(min(la), min(lt))
    hi_r = min(max(la), max(lt))
    pa = fit(la, a_psnr)
    pt = fit(lt, t_psnr)
    bd_psnr = integrate(pt, lo_r, hi_r) - integrate(pa, lo_r, hi_r)
    print(f"bd_rate_pct={bd_rate:.2f} bd_psnr_db={bd_psnr:.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
