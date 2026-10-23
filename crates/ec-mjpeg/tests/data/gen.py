#!/usr/bin/env python3
"""Regenerates the synthetic JPEG fixtures next to this file (Pillow, libjpeg-turbo).

The pictures are computed gradients plus seeded noise; nothing here is personal media.
The expected pixels are never stored: the tests ask ffmpeg's mjpeg decoder at run time."""
import numpy as np
from PIL import Image

def picture(w, h, seed):
    rng = np.random.default_rng(seed)
    y, x = np.mgrid[0:h, 0:w]
    r = (x * 255 // max(w - 1, 1)) % 256
    g = (y * 255 // max(h - 1, 1)) % 256
    b = ((x ^ y) * 5) % 256
    img = np.stack([r, g, b], -1).astype(np.int32)
    img += rng.integers(-24, 25, img.shape)
    img[h // 3: h // 3 + 6, w // 4: w // 4 + 9] = [250, 10, 20]
    return Image.fromarray(np.clip(img, 0, 255).astype(np.uint8), "RGB")

SUB = {"420": 2, "422": 1, "444": 0}
jobs = []
for name, sub in SUB.items():
    jobs.append((f"prog{name}", dict(progressive=True, subsampling=sub, quality=85), (64, 48)))
    jobs.append((f"rst{name}", dict(restart_marker_blocks=2, subsampling=sub, quality=80), (64, 48)))
    jobs.append((f"progrst{name}", dict(progressive=True, restart_marker_blocks=3, subsampling=sub, quality=70), (53, 37)))
    jobs.append((f"odd{name}", dict(subsampling=sub, quality=90, optimize=True), (51, 35)))
jobs.append(("basegray", dict(quality=75), (40, 24), "L"))
jobs.append(("proggray", dict(progressive=True, quality=85), (64, 48), "L"))
jobs.append(("rstgray", dict(restart_marker_blocks=2, quality=80), (61, 45), "L"))
for i, job in enumerate(jobs):
    name, kw, (w, h) = job[:3]
    img = picture(w, h, i + 1)
    if len(job) > 3:
        img = img.convert(job[3])
    img.save(f"{name}.jpg", "JPEG", **kw)
