#!/usr/bin/env python3
"""Draw Space's app icon: a ringed planet on a deep-indigo tile.

Built as an Icon Composer bundle (AppIcon.icon) with separate layers, so macOS 26 renders it
like a native icon: it masks the tile to the system shape and adds its own glass lighting.
Layers, front to back:
  ring-front   the half of the ring that passes in front of the planet
  planet       shaded sphere
  ring-back    the half of the ring behind the planet (drawn dimmer)
  sky          a soft glow behind the planet and a few stars
The background gradient is the icon's fill.

Writes to crates/space-app/resources/:
  AppIcon.icon/            the Icon Composer bundle
  AppIcon.icns, Assets.car compiled from it by Xcode's actool (bundle.sh ships Assets.car)
  AppIcon.png              flattened, pre-masked; the Dock icon for a bare `cargo run` binary
Needs Pillow and Xcode.  Run: python3 scripts/make_app_icon.py
"""
import json, math, os, pathlib, plistlib, random, shutil, subprocess, tempfile
from PIL import Image, ImageDraw, ImageFilter

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer"
S = 4          # supersampling
N = 1024 * S   # full-bleed canvas; the system squircle covers all of it

SKY_TOP, SKY_BOTTOM = (40, 42, 104), (7, 8, 26)
# Planet shading, from the lit point outward.
PLANET_STOPS = [(0.0, (252, 250, 255)), (0.16, (204, 194, 255)), (0.42, (128, 104, 248)), (0.72, (66, 44, 178)), (1.0, (22, 14, 70))]
RING_LEFT, RING_RIGHT = (255, 204, 128), (255, 104, 158)

CX, CY, R = 512 * S, 524 * S, 228 * S                        # planet
RX, RY, TILT, RING_W = 418 * S, 116 * S, -20.0, 42 * S       # ring ellipse


def lerp(a, b, t):
    return tuple(int(round(x + (y - x) * t)) for x, y in zip(a, b))


def stops_lut(stops):
    """256-entry RGB palette from gradient stops."""
    lut = []
    for i in range(256):
        t = i / 255
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t <= t1:
                lut.append(lerp(c0, c1, (t - t0) / (t1 - t0)))
                break
    return lut


def sky():
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    # Soft violet glow behind the planet.
    glow = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse([CX - R * 1.55, CY - R * 1.55, CX + R * 1.55, CY + R * 1.55], fill=(120, 96, 255, 70))
    img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(110 * S)))
    # A few stars, kept clear of the planet and ring.
    rng = random.Random(7)
    d = ImageDraw.Draw(img)
    placed = 0
    while placed < 10:
        x, y = rng.uniform(150, 874) * S, rng.uniform(150, 874) * S
        if math.hypot(x - CX, y - CY) < R + 150 * S:
            continue
        r = rng.choice([3, 4, 4, 5, 6]) * S
        d.ellipse([x - r, y - r, x + r, y + r], fill=(255, 255, 255, rng.randint(110, 220)))
        placed += 1
    return img


def planet():
    # Distance from the lit point (upper left), mapped through the shading stops.
    hx, hy, reach = CX - 0.42 * R, CY - 0.46 * R, 1.9 * R
    size = int(reach * 2)
    grad = Image.radial_gradient("L").resize((size, size), Image.BILINEAR)
    field = Image.new("L", (N, N), 255)
    field.paste(grad, (int(hx - reach), int(hy - reach)))
    lut = stops_lut(PLANET_STOPS)
    shaded = field.convert("P")
    shaded.putpalette([c for rgb in lut for c in rgb])
    shaded = shaded.convert("RGBA")
    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).ellipse([CX - R, CY - R, CX + R, CY + R], fill=255)
    out = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    out.paste(shaded, (0, 0), mask)
    # The ring's shadow on the planet: the front half of the ring, shifted down, soft and dark.
    shadow = ring_path_image(front=True, color=(10, 6, 40), width=RING_W * 1.3, dy=30 * S)
    shadow = shadow.filter(ImageFilter.GaussianBlur(10 * S))
    alpha = Image.composite(shadow.getchannel("A").point(lambda v: v * 0.55), Image.new("L", (N, N), 0), mask)
    shade = Image.new("RGBA", (N, N), (10, 6, 40, 0))
    shade.putalpha(alpha)
    out.alpha_composite(shade)
    return out


def ring_path_image(front, color, width, dy):
    """One half of the ring's path in a single color (used for its shadow)."""
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    a = math.radians(TILT)
    pts = []
    for i in range(721):
        t = 2 * math.pi * i / 720
        if (math.sin(t) > 0) != front:
            continue
        ex, ey = RX * math.cos(t), RY * math.sin(t)
        pts.append((CX + ex * math.cos(a) - ey * math.sin(a), CY + dy + ex * math.sin(a) + ey * math.cos(a)))
    d.line(pts, fill=color + (255,), width=int(width), joint="curve")
    return img


def ring(front):
    """Half of the tilted ring: the front half (sin t > 0, nearer the viewer) or the back half."""
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    a = math.radians(TILT)
    steps = 720
    pts = []
    for i in range(steps + 1):
        t = 2 * math.pi * i / steps
        ex, ey = RX * math.cos(t), RY * math.sin(t)
        x, y = CX + ex * math.cos(a) - ey * math.sin(a), CY + ex * math.sin(a) + ey * math.cos(a)
        pts.append((t, x, y))
    for (t0, x0, y0), (t1, x1, y1) in zip(pts, pts[1:]):
        if (math.sin((t0 + t1) / 2) > 0) != front:
            continue
        u = (x0 - (CX - RX)) / (2 * RX)
        col = lerp(RING_LEFT, RING_RIGHT, min(max(u, 0), 1))
        if not front:
            col = lerp(col, (60, 40, 110), 0.35)
        d.line([(x0, y0), (x1, y1)], fill=col + (255,), width=RING_W)
        # Round joins.
        d.ellipse([x1 - RING_W / 2, y1 - RING_W / 2, x1 + RING_W / 2, y1 + RING_W / 2], fill=col + (255,))
    return img


def background():
    img = Image.new("RGBA", (N, N))
    d = ImageDraw.Draw(img)
    for y in range(0, N, S):
        d.rectangle([0, y, N, y + S], fill=lerp(SKY_TOP, SKY_BOTTOM, y / N) + (255,))
    return img


def down(img):
    return img.resize((1024, 1024), Image.LANCZOS)


def squircle(full):
    """Flattened art masked to the standard macOS icon grid (824px body)."""
    body = 824
    mask = Image.new("L", (body * S, body * S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, body * S - 1, body * S - 1], 186 * S, fill=255)
    mask = mask.resize((body, body), Image.LANCZOS)
    out = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    out.paste(full.resize((body, body), Image.LANCZOS), (100, 100), mask)
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    layers = {"sky": sky(), "ring-back": ring(False), "planet": planet(), "ring-front": ring(True)}

    flat = background()
    for img in layers.values():
        flat.alpha_composite(img)
    squircle(down(flat)).save(OUT / "AppIcon.png")

    bundle = OUT / "AppIcon.icon"
    shutil.rmtree(bundle, ignore_errors=True)
    (bundle / "Assets").mkdir(parents=True)
    for name, img in layers.items():
        down(img).save(bundle / "Assets" / f"{name}.png")
    srgb = lambda c: "srgb:" + ",".join(f"{v / 255:.5f}" for v in c) + ",1.00000"
    front_to_back = ["ring-front", "planet", "ring-back", "sky"]
    (bundle / "icon.json").write_text(json.dumps({
        "fill": {"linear-gradient": [srgb(SKY_TOP), srgb(SKY_BOTTOM)]},
        "groups": [
            # Planet and ring get the system's glass lighting; the sky stays flat.
            {"layers": [{"image-name": f"{n}.png", "name": n} for n in front_to_back[:3]],
             "shadow": {"kind": "neutral", "opacity": 0.5}, "specular": True},
            {"layers": [{"image-name": "sky.png", "name": "sky", "glass": False}],
             "shadow": {"kind": "none", "opacity": 0}, "specular": False},
        ],
        "supported-platforms": {"squares": ["macOS"]},
    }, indent=2) + "\n")

    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, DEVELOPER_DIR=DEVELOPER_DIR)
        partial = pathlib.Path(tmp) / "partial.plist"
        subprocess.run(["xcrun", "actool", str(bundle), "--compile", tmp, "--platform", "macosx",
                        "--minimum-deployment-target", "13.0", "--app-icon", "AppIcon",
                        "--output-partial-info-plist", str(partial), "--output-format", "human-readable-text",
                        "--notices", "--warnings", "--errors"], check=True, env=env, stdout=subprocess.DEVNULL)
        for name in ("Assets.car", "AppIcon.icns"):
            shutil.copy(pathlib.Path(tmp) / name, OUT / name)
    print("wrote", ", ".join(p.name for p in sorted(OUT.iterdir())))


if __name__ == "__main__":
    main()
