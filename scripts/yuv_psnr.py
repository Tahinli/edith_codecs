#!/usr/bin/env python3
"""Per-frame and mean PSNR between two raw I420 files of the same geometry.

Usage: python3 -I yuv_psnr.py <width> <height> <ref.yuv> <test.yuv> [bits]

`bits` is 8 (default) or 10; at 10 the files hold 16-bit little-endian samples
and the peak is 1023.

Prints the mean luma PSNR over frames, the mean PSNR over all three planes
(sample-weighted), and the frame count. Identical files give inf.
"""
import array
import math
import sys


def main() -> int:
    w, h = int(sys.argv[1]), int(sys.argv[2])
    bits = int(sys.argv[5]) if len(sys.argv) > 5 else 8
    peak = (1 << bits) - 1
    cw, ch = (w + 1) // 2, (h + 1) // 2
    sizes = [w * h, cw * ch, cw * ch]
    frame = sum(sizes)
    code = "H" if bits > 8 else "B"
    ref = array.array(code)
    ref.frombytes(open(sys.argv[3], "rb").read())
    test = array.array(code)
    test.frombytes(open(sys.argv[4], "rb").read())
    n = min(len(ref), len(test)) // frame
    if len(ref) != len(test):
        print(f"length differs: ref={len(ref)} test={len(test)}")
    y_psnr, all_sse, all_count = [], 0, 0
    for f in range(n):
        base = f * frame
        sse_planes = []
        off = base
        for size in sizes:
            a = ref[off:off + size]
            b = test[off:off + size]
            sse = sum((x - y) * (x - y) for x, y in zip(a, b))
            sse_planes.append((sse, size))
            off += size
        sse_y, cnt_y = sse_planes[0]
        y_psnr.append(10 * math.log10(peak * peak * cnt_y / sse_y) if sse_y else math.inf)
        all_sse += sum(s for s, _ in sse_planes)
        all_count += frame
    mean_y = sum(y_psnr) / len(y_psnr) if y_psnr else 0.0
    all_psnr = 10 * math.log10(peak * peak * all_count / all_sse) if all_sse else math.inf
    print(f"frames={n} psnr_y={mean_y:.4f} psnr_yuv={all_psnr:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
