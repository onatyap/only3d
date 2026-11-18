# A closed, bumpy sphere with a flat bottom, as a binary STL: the "dense organic mesh" benchmark
# model (an n x n grid gives about 2n^2 triangles; n = 710 is about a million).
#   python3 make_blob.py out.stl 710
import math
import struct
import sys

out, n = sys.argv[1], int(sys.argv[2])
r0 = 30.0


def point(i, j):
    th = math.pi * i / (n - 1)
    ph = 2 * math.pi * j / n
    # bumps vanish at the poles, so the surface closes there
    r = r0 * (1 + 0.08 * math.sin(7 * th) * math.cos(5 * ph) + 0.04 * math.sin(th) * math.sin(13 * ph + 3 * th))
    # flat bottom 14 mm up, so it stands on the bed
    return (r * math.sin(th) * math.cos(ph), r * math.sin(th) * math.sin(ph), max(r0 * 1.2 + r * math.cos(th), 14.0))


pts = [[point(i, j) for j in range(n)] for i in range(n)]
tris = []
for i in range(n - 1):
    for j in range(n):
        a, b, c, d = pts[i][j], pts[i][(j + 1) % n], pts[i + 1][j], pts[i + 1][(j + 1) % n]
        if i > 0:
            tris.append((a, c, b))
        if i < n - 2:
            tris.append((b, c, d))
with open(out, "wb") as f:
    f.write(b"\0" * 80)
    f.write(struct.pack("<I", len(tris)))
    for t in tris:
        f.write(struct.pack("<3f", 0, 0, 0))
        for v in t:
            f.write(struct.pack("<3f", *v))
        f.write(b"\0\0")
print(f"{len(tris)} triangles")
