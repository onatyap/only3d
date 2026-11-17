#!/usr/bin/env python3
"""Draw the only3d app icon (the title-bar pixel mark on a dark tile) and write the PNG, the
Windows .ico and, on macOS, the .icns. Run from anywhere: python3 packaging/icons/make_icons.py
Needs Pillow; the .icns needs macOS's iconutil."""
import os, shutil, subprocess, sys, tempfile
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
CORAL, BLUE, AMBER = (0xF0, 0x90, 0x82), (0x4D, 0x9A, 0xBF), (0xF1, 0xBE, 0x58)
# the stylised "o" with a corner pixel, as theme::pixel_logo draws it
CELLS = [(0, 0, CORAL), (1, 0, CORAL), (2, 0, BLUE), (0, 1, CORAL), (2, 1, BLUE), (0, 2, AMBER), (1, 2, AMBER)]


def draw(size=1024):
    s = 4  # draw large, then shrink, for clean edges
    n = size * s
    im = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    inset = round(n * 0.098)  # macOS icon grid: the tile is ~80% of the canvas
    d.rounded_rectangle([inset, inset, n - inset, n - inset], radius=round(n * 0.18), fill=(0x23, 0x27, 0x2E, 255))
    cell, gap = round(n * 0.166), round(n * 0.022)
    block = cell * 3 + gap * 2
    x0 = y0 = (n - block) // 2
    for cx, cy, c in CELLS:
        x, y = x0 + cx * (cell + gap), y0 + cy * (cell + gap)
        d.rounded_rectangle([x, y, x + cell, y + cell], radius=round(cell * 0.12), fill=c + (255,))
    return im.resize((size, size), Image.LANCZOS)


def main():
    big = draw(1024)
    big.save(os.path.join(HERE, "only3d.png"))
    big.resize((256, 256), Image.LANCZOS).save(os.path.join(HERE, "only3d-256.png"))
    big.save(os.path.join(HERE, "only3d.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    if sys.platform == "darwin" and shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as t:
            iconset = os.path.join(t, "only3d.iconset")
            os.mkdir(iconset)
            for px in (16, 32, 128, 256, 512):
                big.resize((px, px), Image.LANCZOS).save(os.path.join(iconset, f"icon_{px}x{px}.png"))
                big.resize((px * 2, px * 2), Image.LANCZOS).save(os.path.join(iconset, f"icon_{px}x{px}@2x.png"))
            subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(HERE, "only3d.icns")], check=True)
    print("wrote", ", ".join(sorted(f for f in os.listdir(HERE) if f.startswith("only3d."))))


if __name__ == "__main__":
    main()
