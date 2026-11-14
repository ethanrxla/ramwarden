"""Rasterize a warden sprite SVG (48x48 cells, one <rect> per run) to raw RGBA.

The Windows window draws the warden from these bytes rather than an emoji:
Windows renders the ram emoji as a grey sheep, which is not the warden.

    python3 scripts/sprite-rgba.py web/assets/pets/ram.svg assets/warden/ram48.rgba
"""
import re, sys
N = 48
src, dst = sys.argv[1], sys.argv[2]
px = bytearray(N * N * 4)
for x, y, w, h, fill in re.findall(r'<rect x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)" fill="#([0-9a-fA-F]{6})"', open(src).read()):
    r, g, b = (int(fill[i:i + 2], 16) for i in (0, 2, 4))
    for yy in range(int(y), int(y) + int(h)):
        for xx in range(int(x), int(x) + int(w)):
            i = (yy * N + xx) * 4
            px[i:i + 4] = bytes((r, g, b, 255))
open(dst, "wb").write(px)
