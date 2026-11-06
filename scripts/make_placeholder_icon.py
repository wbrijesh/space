#!/usr/bin/env python3
"""Draw Space's PLACEHOLDER app icon (the real logo is yet to come).

A neutral graphite tile with a dashed inner frame and three primitive shapes (circle, square,
triangle): deliberately generic, so nobody mistakes it for a logo.

Writes to crates/space-app/resources/:
  AppIcon.icon/            Icon Composer bundle: one full-bleed square layer. macOS 26 masks it
                           to the system squircle. A legacy .icns that doesn't match that shape
                           gets put on a grey plate, which is why this format is used.
  AppIcon.icns, Assets.car compiled from AppIcon.icon by Xcode's actool (bundle.sh copies them)
  AppIcon.png              pre-masked squircle, set as the Dock icon only for a bare binary
Needs Pillow and Xcode (actool).  Run: python3 scripts/make_placeholder_icon.py
"""
import json, math, os, pathlib, plistlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer"
S = 4          # supersampling
N = 1024 * S   # full-bleed canvas

TOP, BOTTOM = (74, 76, 82), (36, 37, 41)
FRAME = (205, 207, 213, 170)
INK = (232, 234, 239, 255)


def art():
    """Full-bleed square artwork (the system applies the squircle mask)."""
    img = Image.new("RGBA", (N, N))
    d = ImageDraw.Draw(img)
    for y in range(N):
        t = y / N
        d.line([(0, y), (N, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)) + (255,))

    # Dashed inner frame ("something goes here"), inside the mask's safe area.
    fi, fr = 168 * S, 120 * S
    dash, gap, w = 46 * S, 30 * S, 14 * S
    x0, y0, x1, y1 = fi, fi, N - fi, N - fi

    def dashed(p, q):
        (ax, ay), (bx, by) = p, q
        length, n = math.hypot(bx - ax, by - ay), 0.0
        while n < length:
            e = min(n + dash, length)
            d.line([(ax + (bx - ax) * n / length, ay + (by - ay) * n / length),
                    (ax + (bx - ax) * e / length, ay + (by - ay) * e / length)], fill=FRAME, width=w)
            n += dash + gap

    dashed((x0 + fr, y0), (x1 - fr, y0))
    dashed((x0 + fr, y1), (x1 - fr, y1))
    dashed((x0, y0 + fr), (x0, y1 - fr))
    dashed((x1, y0 + fr), (x1, y1 - fr))
    for cx, cy, a0 in [(x0 + fr, y0 + fr, 180), (x1 - fr, y0 + fr, 270), (x1 - fr, y1 - fr, 0), (x0 + fr, y1 - fr, 90)]:
        d.arc([cx - fr, cy - fr, cx + fr, cy + fr], a0 + 14, a0 + 76, fill=FRAME, width=w)

    # Three primitives with heavy outlines, legible at notification size.
    sw, r, gapx, c = 36 * S, 76 * S, 196 * S, N // 2
    d.ellipse([c - gapx - r, c - r, c - gapx + r, c + r], outline=INK, width=sw)
    q = r - 8 * S
    d.rounded_rectangle([c - q, c - q, c + q, c + q], 6 * S, outline=INK, width=sw)
    tx, h = c + gapx, r * 2.0
    top, right, left = (tx, c - h * 0.56), (tx + h * 0.56, c + h * 0.44), (tx - h * 0.56, c + h * 0.44)
    d.polygon([top, right, left], outline=None)
    d.line([top, right, left, top, right], fill=INK, width=sw, joint="curve")
    return img.resize((1024, 1024), Image.LANCZOS)


def squircle_png(full):
    """The art masked to the standard macOS icon grid (824px body), for the bare-binary Dock icon."""
    body = 824
    mask = Image.new("L", (body * S, body * S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, body * S - 1, body * S - 1], 186 * S, fill=255)
    mask = mask.resize((body, body), Image.LANCZOS)
    out = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    out.paste(full.resize((body, body), Image.LANCZOS), (100, 100), mask)
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    full = art()
    squircle_png(full).save(OUT / "AppIcon.png")

    bundle = OUT / "AppIcon.icon"
    shutil.rmtree(bundle, ignore_errors=True)
    (bundle / "Assets").mkdir(parents=True)
    full.convert("RGB").save(bundle / "Assets" / "art.png")
    (bundle / "icon.json").write_text(json.dumps({
        "groups": [{
            "layers": [{"image-name": "art.png", "name": "art"}],
            "shadow": {"kind": "none", "opacity": 0},
            "translucency": {"enabled": False, "value": 0},
            "specular": False,
        }],
        "supported-platforms": {"squares": ["macOS"]},
    }, indent=2) + "\n")

    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, DEVELOPER_DIR=DEVELOPER_DIR)
        partial = pathlib.Path(tmp) / "partial.plist"
        subprocess.run(["xcrun", "actool", str(bundle), "--compile", tmp, "--platform", "macosx",
                        "--minimum-deployment-target", "13.0", "--app-icon", "AppIcon",
                        "--output-partial-info-plist", str(partial), "--output-format", "human-readable-text",
                        "--notices", "--warnings", "--errors"], check=True, env=env)
        print(plistlib.loads(partial.read_bytes()))
        for name in ("Assets.car", "AppIcon.icns"):
            shutil.copy(pathlib.Path(tmp) / name, OUT / name)
    print("wrote", ", ".join(p.name for p in sorted(OUT.iterdir())))


if __name__ == "__main__":
    main()
