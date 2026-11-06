#!/usr/bin/env python3
"""Draw Space's app icon: an orbit, a thick cream ring with a moon cut into it, on solid orange.

One bold shape, one flat colour, high contrast, so it reads at Dock size the way the best
macOS icons do. Depth comes only from macOS 26's own glass lighting: the mark is an Icon
Composer layer over a solid fill, and the system masks it and lights it.

Writes to crates/space-app/resources/:
  AppIcon.icon/            the Icon Composer bundle (fill + one "orbit" layer)
  AppIcon.icns, Assets.car compiled from it by Xcode's actool (bundle.sh ships Assets.car)
  AppIcon.png              flattened, pre-masked; the Dock icon for a bare `cargo run` binary
Needs Pillow and Xcode.  Run: python3 scripts/make_app_icon.py
"""
import json, math, os, pathlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer"
S = 4          # supersampling
N = 1024 * S   # full-bleed canvas; the system squircle covers all of it
C = N // 2

ORANGE = (255, 92, 26)
CREAM = (250, 246, 238)

RING_R = 262      # centre line radius of the ring
RING_W = 96       # ring stroke
MOON_R = 86       # moon radius
MOON_GAP = 34     # cut-out around the moon, so it sits in the ring rather than on it
MOON_ANGLE = -40  # degrees, 0 = right, negative = up


def orbit():
    """The mark on transparency: ring, gap cut out, moon."""
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    r, w = RING_R * S, RING_W * S
    d.ellipse([C - r - w / 2, C - r - w / 2, C + r + w / 2, C + r + w / 2], outline=CREAM + (255,), width=w)
    a = math.radians(MOON_ANGLE)
    mx, my = C + r * math.cos(a), C + r * math.sin(a)
    gap = MOON_R * S + MOON_GAP * S
    d.ellipse([mx - gap, my - gap, mx + gap, my + gap], fill=(0, 0, 0, 0))
    m = MOON_R * S
    d.ellipse([mx - m, my - m, mx + m, my + m], fill=CREAM + (255,))
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
    mark = orbit()

    flat = Image.new("RGBA", (1024, 1024), ORANGE + (255,))
    flat.alpha_composite(mark)
    squircle(flat).save(OUT / "AppIcon.png")

    bundle = OUT / "AppIcon.icon"
    shutil.rmtree(bundle, ignore_errors=True)
    (bundle / "Assets").mkdir(parents=True)
    mark.save(bundle / "Assets" / "orbit.png")
    srgb = lambda c: "srgb:" + ",".join(f"{v / 255:.5f}" for v in c) + ",1.00000"
    (bundle / "icon.json").write_text(json.dumps({
        "fill": {"solid": srgb(ORANGE)},
        "groups": [{
            "layers": [{"image-name": "orbit.png", "name": "orbit"}],
            "shadow": {"kind": "neutral", "opacity": 0.5},
            "translucency": {"enabled": False, "value": 0},
            "specular": True,
        }],
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
