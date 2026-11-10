#!/usr/bin/env python3
"""Draw Space's app icon: a black S on a flat apricot tile.

The S is one even-weight stroke: a bottom bar, a straight stem at 68 degrees and a top bar of
the same length, joined by tight bends, with both ends cut square. The tile is a single colour,
#FFAF54, halfway (in OKLab) between amber #FFD36B and orange #FF8A3D.

The icon is an Icon Composer bundle: the tile colour is its fill and the S is its one layer, so
macOS 26 masks it to its icon shape and lights it with its glass effect. That one bundle covers
every surface macOS draws the app icon on: Dock, app switcher, Finder and notifications.

Writes to crates/space-app/resources/:
  AppIcon.icon/  the Icon Composer bundle (source of truth for the bundled app)
  Assets.car     compiled from it by Xcode's actool; scripts/bundle.sh ships it
  AppIcon.png    flattened and pre-masked; the Dock icon for a bare `cargo run` binary
Needs Pillow and Xcode.  Run: python3 scripts/make_app_icon.py

macOS caches icons per bundle id; if the old one keeps showing:
  rm -rf "$(getconf DARWIN_USER_CACHE_DIR)"/com.apple.{dock.iconcache,iconservices,iconservicesagent}
  killall iconservicesagent Dock NotificationCenter
"""
import json, math, os, pathlib, shutil, subprocess, tempfile
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/space-app/resources"
DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer"
SIZE = 1024
SS = 4  # supersampling for the S edges

TILE = (0xFF, 0xAF, 0x54)
INK = (10, 10, 11)

# The S, in glyph units (scaled into the tile by MARK).
ANGLE = 68.2    # stem angle from horizontal, degrees
BAR = 14.0      # length of each bar, from its outer end to the stem's corner
HEIGHT = 28.9   # distance between the bars' centre lines
BEND = 10.6     # centre-line radius of both bends
WEIGHT = 7.3    # stroke width
MARK = 560      # the S's larger dimension in the 1024 tile, stroke included


def s_outline():
    """Closed outline of the S in tile pixels: the centre line offset by half the weight."""
    dx = HEIGHT / math.tan(math.radians(ANGLE))
    corners = [(0.0, HEIGHT), (BAR, HEIGHT), (BAR + dx, 0.0), (2 * BAR + dx, 0.0)]
    line = centre_line(corners, BEND, step=0.05)
    xs, ys = [p[0] for p in line], [p[1] for p in line]
    k = MARK / max(max(xs) - min(xs) + WEIGHT, max(ys) - min(ys) + WEIGHT)
    cx, cy = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2
    line = [((x - cx) * k + SIZE / 2, (y - cy) * k + SIZE / 2) for x, y in line]
    h = WEIGHT * k / 2
    left, right = [], []
    for i, (x, y) in enumerate(line):
        (ax, ay), (bx, by) = line[max(i - 1, 0)], line[min(i + 1, len(line) - 1)]
        length = math.hypot(bx - ax, by - ay) or 1
        nx, ny = -(by - ay) / length, (bx - ax) / length
        left.append((x + nx * h, y + ny * h))
        right.append((x - nx * h, y - ny * h))
    return left + right[::-1]


def centre_line(corners, radius, step):
    """Polyline through `corners` with each inner corner rounded to `radius`, densely sampled."""
    pts, start = [], corners[0]
    for a, b, c in zip(corners, corners[1:], corners[2:]):
        v1, v2 = unit(a[0] - b[0], a[1] - b[1]), unit(c[0] - b[0], c[1] - b[1])
        turn = math.acos(max(-1.0, min(1.0, v1[0] * v2[0] + v1[1] * v2[1])))
        reach = radius / math.tan(turn / 2)
        p1 = (b[0] + v1[0] * reach, b[1] + v1[1] * reach)
        p2 = (b[0] + v2[0] * reach, b[1] + v2[1] * reach)
        bis = unit(v1[0] + v2[0], v1[1] + v2[1])
        centre = (b[0] + bis[0] * radius / math.sin(turn / 2), b[1] + bis[1] * radius / math.sin(turn / 2))
        pts += segment(start, p1, step)
        a0 = math.atan2(p1[1] - centre[1], p1[0] - centre[0])
        sweep = (math.atan2(p2[1] - centre[1], p2[0] - centre[0]) - a0 + 3 * math.pi) % (2 * math.pi) - math.pi
        n = max(int(abs(sweep) * radius / step), 2)
        pts += [(centre[0] + radius * math.cos(a0 + sweep * i / n), centre[1] + radius * math.sin(a0 + sweep * i / n))
                for i in range(n)]
        start = p2
    return pts + segment(start, corners[-1], step) + [corners[-1]]


def segment(p, q, step):
    n = max(int(math.dist(p, q) / step), 1)
    return [(p[0] + (q[0] - p[0]) * i / n, p[1] + (q[1] - p[1]) * i / n) for i in range(n)]


def unit(x, y):
    length = math.hypot(x, y) or 1
    return x / length, y / length


def s_layer():
    big = Image.new("RGBA", (SIZE * SS, SIZE * SS), (0, 0, 0, 0))
    ImageDraw.Draw(big).polygon([(x * SS, y * SS) for x, y in s_outline()], fill=INK + (255,))
    return big.resize((SIZE, SIZE), Image.LANCZOS)


def premasked(flat):
    """Flattened art on the standard macOS icon grid (824 px body), for the bare-binary Dock icon."""
    body = 824
    mask = Image.new("L", (body * SS, body * SS), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, body * SS - 1, body * SS - 1], 186 * SS, fill=255)
    out = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    out.paste(flat.resize((body, body), Image.LANCZOS), (100, 100), mask.resize((body, body), Image.LANCZOS))
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    mark = s_layer()

    flat = Image.new("RGBA", (SIZE, SIZE), TILE + (255,))
    flat.alpha_composite(mark)
    premasked(flat).save(OUT / "AppIcon.png")

    bundle = OUT / "AppIcon.icon"
    shutil.rmtree(bundle, ignore_errors=True)
    (bundle / "Assets").mkdir(parents=True)
    mark.save(bundle / "Assets" / "s.png")
    (bundle / "icon.json").write_text(json.dumps({
        "fill": {"solid": "srgb:" + ",".join(f"{c / 255:.5f}" for c in TILE) + ",1.00000"},
        "groups": [{"layers": [{"image-name": "s.png", "name": "s"}],
                    "shadow": {"kind": "neutral", "opacity": 0.35}, "specular": True}],
        "supported-platforms": {"squares": ["macOS"]},
    }, indent=2) + "\n")

    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(["xcrun", "actool", str(bundle), "--compile", tmp, "--platform", "macosx",
                        "--minimum-deployment-target", "13.0", "--app-icon", "AppIcon",
                        "--output-partial-info-plist", str(pathlib.Path(tmp) / "partial.plist"),
                        "--errors", "--warnings"],
                       check=True, env=dict(os.environ, DEVELOPER_DIR=DEVELOPER_DIR), stdout=subprocess.DEVNULL)
        shutil.copy(pathlib.Path(tmp) / "Assets.car", OUT / "Assets.car")
    print("wrote AppIcon.icon, Assets.car, AppIcon.png in", OUT.relative_to(ROOT))


if __name__ == "__main__":
    main()
