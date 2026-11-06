#!/usr/bin/env python3
"""Draw Space's PLACEHOLDER app icon (the real logo is yet to come).

A neutral graphite squircle with a dashed inner frame and three primitive shapes
(circle, square, triangle): deliberately generic, so nobody mistakes it for a logo.

Writes crates/space-app/resources/AppIcon.png (1024px) and AppIcon.icns.
Needs Pillow and macOS `iconutil`.  Run: python3 scripts/make_placeholder_icon.py
"""
import math, pathlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
S = 4  # supersampling
N = 1024 * S

def squircle(size, inset, radius):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle([inset, inset, size - inset, size - inset], radius, fill=255)
    return m

def draw():
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    inset, radius = 100 * S, 185 * S  # macOS icon grid: 824px body
    # Soft drop shadow.
    shadow = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    shadow.putalpha(squircle(N, inset, radius).point(lambda a: a * 0.45))
    shadow = shadow.transform((N, N), Image.AFFINE, (1, 0, 0, 0, 1, -12 * S)).filter(ImageFilter.GaussianBlur(18 * S))
    img.alpha_composite(shadow)
    # Body: vertical graphite gradient.
    grad = Image.new("RGBA", (N, N))
    gd = ImageDraw.Draw(grad)
    top, bot = (74, 76, 82), (38, 39, 43)
    for y in range(N):
        t = y / N
        gd.line([(0, y), (N, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)) + (255,))
    body = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    body.paste(grad, (0, 0), squircle(N, inset, radius))
    img.alpha_composite(body)
    d = ImageDraw.Draw(img)
    # Hairline highlight on the rim.
    d.rounded_rectangle([inset, inset, N - inset, N - inset], radius, outline=(255, 255, 255, 40), width=3 * S)
    # Dashed inner frame: "something goes here".
    fi, fr = 190 * S, 110 * S
    dash, gap, w = 34 * S, 22 * S, 7 * S
    col = (200, 202, 208, 150)
    x0, y0, x1, y1 = fi, fi, N - fi, N - fi
    def dashed(p, q):
        (ax, ay), (bx, by) = p, q
        L = math.hypot(bx - ax, by - ay); n = 0.0
        while n < L:
            e = min(n + dash, L)
            d.line([(ax + (bx - ax) * n / L, ay + (by - ay) * n / L), (ax + (bx - ax) * e / L, ay + (by - ay) * e / L)], fill=col, width=w)
            n += dash + gap
    dashed((x0 + fr, y0), (x1 - fr, y0)); dashed((x0 + fr, y1), (x1 - fr, y1))
    dashed((x0, y0 + fr), (x0, y1 - fr)); dashed((x1, y0 + fr), (x1, y1 - fr))
    for cx, cy, a0 in [(x0 + fr, y0 + fr, 180), (x1 - fr, y0 + fr, 270), (x1 - fr, y1 - fr, 0), (x0 + fr, y1 - fr, 90)]:
        d.arc([cx - fr, cy - fr, cx + fr, cy + fr], a0 + 12, a0 + 78, fill=col, width=w)
    # Three primitives, outlined.
    ink, sw = (226, 228, 233, 235), 16 * S
    r, cy, gapx = 82 * S, N // 2, 205 * S
    c = N // 2
    d.ellipse([c - gapx - r, cy - r, c - gapx + r, cy + r], outline=ink, width=sw)
    d.rectangle([c - r + 6 * S, cy - r + 6 * S, c + r - 6 * S, cy + r - 6 * S], outline=ink, width=sw)
    tx, h = c + gapx, r * 1.85
    tri = [(tx, cy - h * 0.58), (tx + h * 0.56, cy + h * 0.42), (tx - h * 0.56, cy + h * 0.42)]
    d.line(tri + [tri[0], tri[1]], fill=ink, width=sw, joint="curve")
    return img.resize((1024, 1024), Image.LANCZOS)

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    icon = draw()
    icon.save(OUT / "AppIcon.png")
    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for px in (16, 32, 128, 256, 512):
            icon.resize((px, px), Image.LANCZOS).save(iconset / f"icon_{px}x{px}.png")
            icon.resize((px * 2, px * 2), Image.LANCZOS).save(iconset / f"icon_{px}x{px}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(OUT / "AppIcon.icns")], check=True)
    print("wrote", OUT / "AppIcon.png", "and AppIcon.icns")

if __name__ == "__main__":
    main()
