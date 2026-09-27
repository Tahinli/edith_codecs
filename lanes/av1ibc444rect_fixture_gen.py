import sys, random
W, H = 320, 240
rng = random.Random(60606)
y = bytearray(W*H); u = bytearray(W*H); v = bytearray(W*H)
def base(x, y):
    return ((x*3 + y*5) ^ (x*11 + y*13)) & 63
def wm(wx, wy):                      # per-window watermark: unique per window
    return ((wx*73) ^ (wy*151) ^ ((wx*wy) & 255)) & 63
for j in range(H):
    for i in range(W):
        b = (base(i, j) ^ wm(i >> 3, j >> 3)) + 32
        y[j*W+i] = b
        u[j*W+i] = 128 + ((base(i+37, j+91) ^ wm((i+37) >> 3, (j+91) >> 3)) & 15) - 7
        v[j*W+i] = 128 - ((base(i+13, j+57) ^ wm((i+13) >> 3, (j+57) >> 3)) & 15) - 7
patch = [[96 + rng.randrange(64) for _ in range(8)] for _ in range(8)]
def put(X, Y, luma, cuv):
    y[Y*W+X] = min(255, max(0, luma))
    u[Y*W+X] = min(255, max(0, cuv))
    v[Y*W+X] = min(255, max(0, 255 - cuv))
for by in range(0, H, 8):
    for bx in range(0, W, 8):
        r = rng.random()
        if r < 0.55:
            shape = (8, 4, 0 if rng.random() < 0.5 else 4)
        elif r < 0.85:
            shape = (4, 8, 0)
            if bx + 4 >= W: shape = (8, 4, rng.randrange(2)*4)
        else:
            continue
        w, h, oy = shape
        bias = rng.randrange(-3, 4)
        flips = {(rng.randrange(w), rng.randrange(h)) for _ in range(5)}
        for j in range(h):
            for i in range(w):
                s = patch[(by+oy+j) % 8][(bx+i) % 8]
                if (i, j) in flips: s ^= 1
                put(bx+i, by+oy+j, s + bias, 100 + (s & 31) + bias)
with open(sys.argv[1], 'wb') as f:
    f.write(b'YUV4MPEG2 W320 H240 F25:1 Ip A1:1 C444 XYSCSS=444\nFRAME\n')
    f.write(bytes(y)); f.write(bytes(u)); f.write(bytes(v))
